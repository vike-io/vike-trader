//! **The rendered, redacted settings-file rows** — the FILES half of `vike-cli config show`,
//! MOVED down from `crates/vike-cli/src/cmd/config.rs` (where its printers still live) so a second
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
//! What deliberately did NOT move: the printers (`crates/vike-cli/src/cmd/config.rs`'s
//! `print_file_table` and `settings_json`) — rendering is each surface's own job — and the ENV
//! half (`Resolved`, built over `vike_ops::settings::SETTINGS`), which this crate cannot see:
//! vike-ops sits far above it. The wire verb serves the FILES half only, on purpose — see
//! `crates/vike-tradehub/src/server.rs`'s `SettingsShowSource` for that scope argument.
//!
//! INVARIANT (the reason this module exists): redaction happens ON CONSTRUCTION, in
//! [`resolve_file_row`], so no printer — CLI table, `--json`, wire frame, GUI grid — can leak a
//! secret-shaped value. `a_secret_settings_value_never_enters_the_file_row` below pins it.

use crate::consumed;
use crate::provenance::{Description, Origin, ResolvedSetting};
use crate::redact::{is_secret_key, REDACTED, SET, UNSET};

/// One settings-file row, redacted and rendered — the file twin of `vike-cli`'s env-half
/// `Resolved`, and the same invariant for the same reason: redaction happens here, on
/// construction, so no printer can leak.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileRow {
    /// The dotted key (`config.tradehub_addr`) — its first segment is the [`crate::Settings`]
    /// section.
    pub key: String,
    /// The settings FILE this key belongs to (`policy.toml`, `config.toml`, …) — carried through
    /// from [`ResolvedSetting::file`] (the one field the pre-move `vike-cli` row dropped, because
    /// its table did not group; the wire consumer and the GUI grid both do).
    pub file: &'static str,
    /// The layer that set it, as one cell (`default`, `policy.toml`, `env:VIKE_RECONCILE`).
    pub origin: String,
    /// The machine word: `default` / `file` / `env`.
    pub origin_kind: &'static str,
    /// The effective value, rendered — ALREADY redacted when `secret`.
    pub value: String,
    /// The compiled-in default, rendered — ALREADY redacted when `secret`.
    pub default: String,
    /// The effective value is not what the winning layer holds — a later rule moved it (today,
    /// only the preference-over-policy clamp, whose warning names both).
    pub adjusted: bool,
    /// The key is credential-shaped ([`is_secret_key`]) and `value`/`default` are the redaction
    /// sentinels, never store bytes.
    pub secret: bool,
    /// Whether something OUTSIDE the settings system reads this key and acts on it
    /// ([`crate::is_consumed`]).
    ///
    /// This is the whole reason [`crate::CONSUMPTION`] is `pub` data rather than a test
    /// fixture. Until it existed, a settings file that validated and was accepted by
    /// `deny_unknown_fields` was reported HERE as the ORIGIN of an effective value even when
    /// nothing read it — positive confirmation of something false, which is strictly worse than an
    /// unimplemented feature, because an unimplemented feature has no output claiming it works.
    /// Five keys were in exactly that state when this column was added.
    pub consumed: bool,
    /// WHICH BINARY reads it ([`crate::Consumer::binary`]) — `None` for an unread key, and
    /// also for a library consumer, whose read belongs to every binary that links it.
    ///
    /// `consumed` alone was not enough, and a clean install found the gap: on a headless tradehub
    /// or recorder box, `config.state_dir`, `config.store_root` and `preferences.chart_style` all
    /// reported `READ: yes` while their only reader is `vike-app`, the GUI, which never executes
    /// there. Setting `config.state_dir` on a daemon box was measured to do nothing at all —
    /// correct behaviour (it is vike-app's strategy-state sidecar) reported as if it had worked.
    /// `state_dir` is also the obvious name for the directory the README calls `settings/state/`,
    /// so the row an operator lands on is the one that means something else.
    pub read_by: Option<&'static str>,
    /// For an unread key, what reads its environment variable instead. `None` when the key IS
    /// consumed. Printed under the table rather than as a column — it is a paragraph, not a cell.
    pub why_unread: Option<&'static str>,
}

impl FileRow {
    /// The `READ` cell: the consuming BINARY when one program owns the read, `yes` when a library
    /// does (no single honest name), `NO` when nothing reads it at all.
    pub fn read_cell(&self) -> &str {
        match (self.consumed, self.read_by) {
            (false, _) => "NO",
            (true, Some(binary)) => binary,
            (true, None) => "yes",
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
    let consumer = consumed::consumer_of(&row.key);
    FileRow {
        key: row.key.clone(),
        file: row.file,
        origin: row.origin.label(),
        origin_kind: row.origin.kind(),
        value,
        default,
        adjusted: row.adjusted,
        secret,
        // A key the table does not carry (`policy.*`, which has its own gate) is NOT reported as
        // unread — that would warn about ceilings which are enforced.
        consumed: consumer.is_none_or(consumed::Consumer::is_consumed),
        read_by: consumer.and_then(consumed::Consumer::binary),
        why_unread: consumer.and_then(consumed::Consumer::why_not),
    }
}

/// The files half, filtered and rendered. `filter` is a case-insensitive substring match on the
/// dotted key or the settings file; `changed_only` drops every row nothing configured.
pub fn file_rows(d: &Description, filter: Option<&str>, changed_only: bool) -> Vec<FileRow> {
    let needle = filter.map(str::to_ascii_lowercase);
    d.rows
        .iter()
        .filter(|r| match needle.as_deref() {
            None => true,
            Some(n) => r.key.to_ascii_lowercase().contains(n) || r.file.contains(n),
        })
        .filter(|r| !changed_only || r.origin != Origin::Default)
        .map(resolve_file_row)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn map(pairs: &[(&str, &str)]) -> HashMap<String, String> {
        pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect()
    }

    /// Whatever a settings file holds for a credential-shaped key, no byte of it reaches the row —
    /// the invariant every disclosure surface (CLI table, `--json`, the tradehub wire) leans on.
    #[test]
    fn a_secret_settings_value_never_enters_the_file_row() {
        const LEAK: &str = "sk-do-not-print-me";
        let secret = ResolvedSetting {
            key: "config.bot_token".to_string(),
            file: "config.toml",
            value: Some(LEAK.to_string()),
            default: None,
            origin: Origin::File("config.toml"),
            origin_value: Some(LEAK.to_string()),
            adjusted: false,
        };
        let r = resolve_file_row(&secret);
        assert!(r.secret);
        assert_eq!(r.value, SET);
        assert!(!format!("{r:?}").contains(LEAK), "the secret leaked into {r:?}");
    }

    /// Every typed setting reaches the table, and with no settings directory every one of them is
    /// honestly reported as a compiled-in default — the loudest possible answer to "did my
    /// policy.toml get read?".
    #[test]
    fn the_files_half_covers_every_typed_setting() {
        let d = crate::describe(None, &map(&[])).unwrap();
        let rows = file_rows(&d, None, false);
        assert_eq!(rows.len(), crate::provenance::setting_keys().len());
        assert!(rows.iter().any(|r| r.key == "policy.max_notional_per_order"));
        assert!(rows.iter().any(|r| r.key == "preferences.log_file_level"));
        assert!(rows.iter().any(|r| r.key == "flags.reconcile"));
        assert!(rows.iter().all(|r| r.origin == "default"), "no file was read");
        assert!(file_rows(&d, None, true).is_empty(), "--changed-only hides every default");
    }

    #[test]
    fn the_files_filter_matches_the_key_or_the_file() {
        let d = crate::describe(None, &map(&[])).unwrap();
        assert!(file_rows(&d, Some("policy"), false).iter().all(|r| r.key.starts_with("policy.")));
        assert!(!file_rows(&d, Some("flags.toml"), false).is_empty());
        assert!(file_rows(&d, Some("no-such-key"), false).is_empty());
    }

    /// An environment override reaches the files half AND names its variable — the row an operator
    /// checks after wondering why a flag is on.
    #[test]
    fn an_environment_override_is_reported_with_its_variable() {
        let d = crate::describe(None, &map(&[("VIKE_RECONCILE", "1")])).unwrap();
        let rows = file_rows(&d, Some("flags.reconcile"), false);
        let exact = rows.iter().find(|r| r.key == "flags.reconcile").expect("the exact row");
        assert_eq!(exact.value, "true");
        assert_eq!(exact.origin, "env:VIKE_RECONCILE");
        assert_eq!(exact.origin_kind, "env");
        // and its unset siblings came along, still reporting the truth
        assert!(rows.iter().any(|r| r.key == "flags.reconcile_balance" && r.origin == "default"));
    }

    /// The `READ` cell's three words, from the three `(consumed, read_by)` shapes.
    #[test]
    fn the_read_cell_names_the_binary_a_library_or_nothing() {
        let base = resolve_file_row(&ResolvedSetting {
            key: "config.tradehub_addr".to_string(),
            file: "config.toml",
            value: None,
            default: None,
            origin: Origin::Default,
            origin_value: None,
            adjusted: false,
        });
        // The real consumption table drives the real rows; pin the rendering rule itself on
        // synthesized field states so the three arms stay covered whatever the table says.
        let mut r = base;
        r.consumed = false;
        r.read_by = None;
        assert_eq!(r.read_cell(), "NO");
        r.consumed = true;
        assert_eq!(r.read_cell(), "yes");
        r.read_by = Some("tradehub");
        assert_eq!(r.read_cell(), "tradehub");
    }
}
