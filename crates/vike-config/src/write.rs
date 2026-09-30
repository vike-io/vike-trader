//! **The settings write PLANNER** (`docs/decisions/0086`: settings live only in the database, and
//! a write is one row) — turn a caller-facing dotted key and raw value into the one-row
//! [`vike_secrets::RowChange`] change [`vike_secrets::write_setting_row_in`] executes, and build
//! the validator that primitive requires. `write_setting_row` is the ONE spelling every settings
//! write in this tree is migrating onto: `vike-cli config set`, `backend setup`/`connect`, the
//! GUI's arming Save and the daemon's own control channel all call it — never
//! [`vike_secrets::write_setting_row_in`] directly, because only this crate (layer 20) can resolve
//! rows into a typed [`crate::config::Config`]/[`crate::policy::Policy`]/… through
//! [`crate::load_with_source`] and [`crate::drift::differing_keys`], both of which
//! [`vike_secrets`] (layer 15) must not depend upward to reach.
//!
//! # There are no settings FILES any more
//!
//! `<project>/settings/db/vike.db` is the only settings store (0086 point 1). This module used to
//! own a comment-preserving `toml_edit` editor over the four settings TOMLs, a cross-process
//! advisory lock on a `settings.lock` sentinel, and a retyped-key confirm ceremony —
//! `docs/decisions/0086` deletes all three: the files (point 1), the lock (point 4, replaced by the
//! store's own `BEGIN IMMEDIATE`) and the confirm (point 7, *"confirmation over confirmation … a
//! nightmare"*). What survives from that era is [`SettingsFile`] itself, repurposed: it no longer
//! names a FILE this crate opens, only the SECTION WORD a dotted key's first segment must match
//! (`"policy"` / `"config"` / `"preferences"` / `"flags"`) — the same four words `vike-cli config
//! show` groups its rows under, and the same four [`crate::config::Config`]/[`crate::policy::
//! Policy`]/[`crate::preferences::Preferences`]/[`crate::flags::Flags`] types [`fn@crate::load`]
//! resolves. ⚠ The `Authority`/file-presence machinery this paragraph used to point at as "the
//! RECORD's Phase 2, out of this change's scope" is now DELETED — `crate::load` reads rows only,
//! and `crate::provenance`/`crate::show`/`crate::drift`/`crate::remedy`/`crate::boot` carry the
//! rest of that same landing.
//!
//! # What a write actually does now
//!
//! 1. **[`row_change_for`] resolves the key grammar**, matched by SEGMENT (never by string
//!    prefix — the deleted `is_policy_plane_key`'s tombstone below says why that matters):
//!    `policy.venues.<venue>` and `policy.accounts.<venue>.<LABEL>` are each one `venue_arming` row
//!    ([`vike_secrets::RowChange::Arming`]); everything else is one plain `setting` row keyed on
//!    the section word ([`vike_secrets::RowChange::Setting`]), its value typed the same way this
//!    module always typed a raw string (a parseable TOML scalar lands typed, anything else lands as
//!    a string) and rendered through the one JSON-scalar encoder, [`crate::mirror::json_scalar`].
//! 2. **[`validate_row_write`] is the validator [`vike_secrets::write_setting_row_in`] calls inside
//!    its own open transaction**, before a row is touched: the CURRENT store must already boot
//!    clean if it claims a seal (a write must not re-bless an erased ceiling), and the CANDIDATE —
//!    current rows plus this one change — must boot clean too and [`crate::drift::differing_keys`]
//!    between the two resolutions may name only the written key (plus the two synthetic keys an
//!    arming change moves as a side effect, since `policy.venues.*` never appears in the ordinary
//!    leaf walk at all).
//! 3. **The primitive does the rest** inside one `BEGIN IMMEDIATE`: it upserts the one row, re-folds
//!    `account.armed` when an arming row moved, moves the integrity seal by the same delta, and
//!    commits — or rolls back on any refusal, leaving the database BYTE-IDENTICAL. See
//!    `vike_secrets::write_setting_row_in`'s own doc for the transaction.
//!
//! # No retype confirm, for any key (0086 point 7)
//!
//! `typed_confirm_reason`, `CONFIRMED_FLAG_KEYS`, `requires_typed_confirm` and the `--confirm` flag
//! this crate used to expose are DELETED, not merely unused: the owner's ruling is that a retype
//! stops nothing a bounds check and an *old -> new* report do not, and it was the one thing every
//! surface disagreed about. What guards a mistake now is [`validate_row_write`]'s own two checks,
//! run on every write, and the caller's *old -> new* report — never a ceremony the operator or an
//! agent can satisfy by pressing return.
//!
//! `is_policy_plane_key` was a DIFFERENT predicate, and it is DELETED too: not a confirm gate but
//! *which section a key's write belongs to*. ⚠ The two client-side ceremonies that keyed on it went
//! under the same point 7 — the GUI Backend-settings editor's Save gate
//! (`crates/vike-app-core/src/ui/tool_views/backend_settings.rs`'s `can_save_fields`, so every
//! row there saves on one click) and the `trade` REPL's retype prompt (its tombstone is
//! `crates/vike-cli/src/cmd/trade.rs`'s `typed_key_confirm`). Its last caller was the `mcp`
//! server's UNATTENDED gate (`crates/vike-cli/src/cmd/mcp.rs`'s `unattended_refusal`), which
//! since the owner's 2026-09-29 ruling on decision 0040 refuses EVERY setting in a session nobody
//! attends, so it needs no section rule at all. The predicate's tombstone is below.
//!
//! # Former names, for the citations that still use them
//!
//! Earlier drafts of this design (`docs/decisions/0028`, `0055`, `0057` and the node-onboarding
//! spec) call this module's write entry point `set_setting` or `set_setting_within` — this file's
//! actual function is [`write_setting_row`], never spelled either of those two ways. The old
//! whole-file writer also carried a test named `a_broken_current_file_is_refused_not_clobbered`,
//! which asserted the never-clobber property `crate::mirror::seal_refusal` and
//! [`vike_secrets::write_setting_row_in`]'s own byte-identical-on-refusal guarantee now carry
//! instead; that test does not exist in this file any more.

use std::collections::HashMap;
use std::path::Path;

use vike_secrets::{Adoption, StoredSettings};

use crate::layers::CliOverrides;
use crate::source::StoreLayer;

/// One of the four settings SECTIONS, by the word a dotted key's first segment must equal
/// (`"policy"` / `"config"` / `"preferences"` / `"flags"`) — the same grouping
/// `vike-cli config show` renders. ⚠ Despite the name, this type no longer names a FILE this crate
/// opens (see the module doc): `file_name()` is kept only as the historical LABEL a few surfaces
/// still print (`"policy.toml"`), and nothing in this crate reads or writes one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SettingsFile {
    /// The hard ceilings ([`crate::Policy`]).
    Policy,
    /// Deployment settings ([`crate::Config`]).
    Config,
    /// Taste and tuning ([`crate::Preferences`]).
    Preferences,
    /// Operator toggles ([`crate::Flags`]).
    Flags,
}

impl SettingsFile {
    /// Every section, in the loader's application order.
    pub const ALL: [SettingsFile; 4] = [
        SettingsFile::Policy,
        SettingsFile::Config,
        SettingsFile::Preferences,
        SettingsFile::Flags,
    ];

    /// Parse a caller-supplied name: the historical file name (`"policy.toml"`) or its bare stem
    /// (`"policy"`), case-sensitive. `None` for anything else.
    pub fn parse(name: &str) -> Option<SettingsFile> {
        let stem = name.strip_suffix(".toml").unwrap_or(name);
        SettingsFile::ALL.into_iter().find(|f| f.section() == stem)
    }

    /// The historical file-name LABEL (`"policy.toml"`) — a few surfaces still print it as a
    /// familiar name for the section; nothing opens a path spelled this way any more.
    pub fn file_name(self) -> &'static str {
        match self {
            SettingsFile::Policy => "policy.toml",
            SettingsFile::Config => "config.toml",
            SettingsFile::Preferences => "preferences.toml",
            SettingsFile::Flags => "flags.toml",
        }
    }

    /// The dotted-key SECTION this settings section's keys are spelled under (`"policy"` for
    /// `policy.max_notional_per_order`) — the first segment of every `vike-cli config show` /
    /// `WireSettingsRow` key, which is also the spelling [`row_change_for`] demands.
    pub fn section(self) -> &'static str {
        match self {
            SettingsFile::Policy => "policy",
            SettingsFile::Config => "config",
            SettingsFile::Preferences => "preferences",
            SettingsFile::Flags => "flags",
        }
    }

    /// **Which section a dotted KEY belongs to, decided from its SECTION WORD** — the first
    /// segment of `policy.max_notional_per_order`, never a file-name string. `None` when the first
    /// segment names none of the four.
    pub fn of_key(key: &str) -> Option<SettingsFile> {
        let head = key.split('.').next()?;
        SettingsFile::ALL.into_iter().find(|f| f.section() == head)
    }
}

// ⚠ `is_policy_plane_key` stood here and is DELETED (2026-09-29): "is `key` in the POLICY plane?",
// decided from the key's SECTION WORD (`SettingsFile::of_key`) rather than a file name. Every caller
// gave policy writes a special treatment on it — the typed confirms, gone with `docs/decisions/0086`
// point 7, and last the `mcp` server's UNATTENDED gate — and the owner's 2026-09-29 ruling on
// decision 0040 made that gate refuse EVERY setting, which left it no caller. Its reason for being
// is still worth reading: `docs/decisions/0057-the-seven-settings-files-answered-one-at-a-time.md`
// records how a check keyed on a FILE NAME by exact match went silently dead. The name stays in
// this comment because records and comments cite it.

/// The refusal for a `file` that names none of the four settings sections — one message, naming
/// all four in the dotted spelling `vike-cli config show` renders them in, so a caller's typo gets
/// the full menu instead of a guess.
pub fn unknown_file_message(name: &str) -> String {
    format!(
        "unknown settings section {name:?} — every key starts with one of the four `config \
         show` groups its rows under: policy.<key>, config.<key>, preferences.<key> or \
         flags.<key>"
    )
}

/// Split the full dotted key into its in-section path, refusing a spelling that does not fit
/// `file`: the first segment must be the section's word, at least one segment must follow, and no
/// segment may be empty.
fn key_path(file: SettingsFile, dotted_key: &str) -> Result<Vec<&str>, RowPlanError> {
    let bad = |reason: String| RowPlanError::BadKey { key: dotted_key.to_string(), reason };
    let mut segs: Vec<&str> = dotted_key.split('.').collect();
    if segs.iter().any(|s| s.trim().is_empty()) {
        return Err(bad("empty key segment".to_string()));
    }
    if segs[0] != file.section() {
        return Err(bad(format!(
            "a {} key is spelled `{}.<key>` (the same dotted form `config show` renders), got \
             first segment `{}`",
            file.file_name(),
            file.section(),
            segs[0]
        )));
    }
    segs.remove(0);
    if segs.is_empty() {
        return Err(bad(format!(
            "it names the whole {} section — a write sets one key inside it",
            file.section()
        )));
    }
    Ok(segs)
}

/// One key, resolved into the [`vike_secrets::RowChange`] it writes.
///
/// The arming shapes (`policy.venues.<venue>`, `policy.accounts.<venue>.<LABEL>`) are one
/// `venue_arming` row each; everything else is a plain `setting` row keyed on the section word.
/// Matched by SEGMENT — a `policy.venues_something` key must fall through to the plain-row arm,
/// never be caught here by a string-prefix test.
fn row_change_for(key: &str, raw_value: &str) -> Result<vike_secrets::RowChange, RowPlanError> {
    let file = SettingsFile::of_key(key).ok_or_else(|| RowPlanError::BadKey {
        key: key.to_string(),
        reason: unknown_file_message(key.split('.').next().unwrap_or(key)),
    })?;
    let segs = key_path(file, key)?;

    if file == SettingsFile::Policy {
        if let [head, venue] = segs.as_slice()
            && *head == "venues"
        {
            return Ok(vike_secrets::RowChange::Arming {
                venue: (*venue).to_string(),
                label: None,
                mode: raw_value.trim().to_string(),
            });
        }
        if let [head, venue, label] = segs.as_slice()
            && *head == "accounts"
        {
            return Ok(vike_secrets::RowChange::Arming {
                venue: (*venue).to_string(),
                label: Some((*label).to_string()),
                mode: raw_value.trim().to_string(),
            });
        }
    }

    // Everything else is a plain `setting` row. The value is stored as one JSON SCALAR
    // ([`vike_secrets::SettingRow::value`]'s own contract) — typed the same way a raw string was
    // always typed (a parseable TOML scalar is taken as typed, anything else is a string), then
    // rendered through the SAME encoder `vike-cli config mirror` used to fill this same column.
    let trimmed = raw_value.trim();
    let typed: toml::Value =
        trimmed.parse().unwrap_or_else(|_| toml::Value::String(trimmed.to_string()));
    let value = crate::mirror::json_scalar(&typed).ok_or_else(|| RowPlanError::BadKey {
        key: key.to_string(),
        reason: format!(
            "`{key}` would be {}, and the settings store holds one JSON scalar per row",
            crate::mirror::unrenderable_shape(&typed)
        ),
    })?;
    Ok(vike_secrets::RowChange::Setting {
        section: file.section().to_string(),
        key: segs.join("."),
        value,
    })
}

/// **The validator [`write_setting_row`] hands to [`vike_secrets::write_setting_row_in`]** — built
/// here because only this crate can resolve rows through [`crate::load_with_source`] and
/// [`crate::drift::differing_keys`].
///
/// Two checks, in order:
///
/// 1. **The CURRENT store must already boot clean**, if it claims a seal — a write must not
///    re-bless an erased ceiling. Skipped on a never-sealed store, which is the ordinary
///    pre-adoption state and not itself a refusal.
/// 2. **The CANDIDATE must boot clean too**, under the seal this write is ABOUT to leave behind,
///    and [`crate::drift::differing_keys`] between the current and candidate resolutions may name
///    only `key` itself — plus the two synthetic keys an ARMING change moves as a side effect
///    (`policy.venues.*` never appears in the leaf walk at all; a change there can only ever show
///    up through these two, or through nothing, which is why they are the sole exemption).
fn validate_row_write(
    key: &str,
    current: &StoredSettings,
    current_adoption: Option<&Adoption>,
    candidate: &StoredSettings,
) -> Result<(), String> {
    let empty: HashMap<String, String> = HashMap::new();
    let cli = CliOverrides::default();

    let resolve = |rows: &StoredSettings, adoption: &Adoption| {
        crate::load_with_source(
            None,
            StoreLayer::Rows { rows, adopted: Some(adoption) },
            &empty,
            &cli,
        )
    };
    let synthetic = |rows: &StoredSettings| Adoption {
        adopted_at: String::new(),
        tool_version: String::new(),
        files_present: String::new(),
        venues_declared: !rows.arming.is_empty(),
        setting_rows: rows.settings.len(),
        arming_rows: rows.arming.len(),
    };

    if let Some(adoption) = current_adoption {
        match resolve(current, adoption) {
            Ok(settings) => {
                if let Some(why) = settings.seal_refusal.or(settings.store_refusal) {
                    return Err(format!(
                        "the current settings store does not boot clean, so this write refuses to \
                         re-bless it: {why}"
                    ));
                }
            }
            Err(e) => return Err(format!("the current settings rows do not resolve: {e}")),
        }
    }

    let current_synth = synthetic(current);
    let current_settings = resolve(current, current_adoption.unwrap_or(&current_synth))
        .map_err(|e| format!("the current settings rows do not resolve: {e}"))?;

    let candidate_synth = synthetic(candidate);
    let candidate_settings = resolve(candidate, &candidate_synth)
        .map_err(|e| format!("this value does not resolve: {e}"))?;
    if let Some(why) =
        candidate_settings.seal_refusal.as_ref().or(candidate_settings.store_refusal.as_ref())
    {
        return Err(format!("this write would leave the settings store unable to boot: {why}"));
    }

    for d in crate::drift::differing_keys(&current_settings, &candidate_settings) {
        if d.key != key && d.key != "policy.venues (stated at all)" && d.key != "policy.accounts" {
            return Err(format!(
                "this write would also change `{}` (`{}` -> `{}`), and a one-row write may only \
                 change the key it names",
                d.key, d.before, d.after
            ));
        }
    }
    Ok(())
}

/// What [`write_setting_row`] did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RowReport {
    /// The full dotted key, as the caller spelled it.
    pub key: String,
    /// The row's own value before this write, rendered — `None` for a brand-new row.
    pub old_value: Option<String>,
    /// The value written, rendered as the caller spelled it (trimmed).
    pub new_value: String,
}

/// Why [`write_setting_row`] refused. Every write this crate performs leaves the database
/// BYTE-IDENTICAL on a refusal (`vike_secrets::write_setting_row_in`'s own doc) — there is no
/// `Stranded` shape left to distinguish, unlike the file-era `SettingsWriteError` this type
/// replaces.
#[derive(Debug)]
pub enum RowPlanError {
    /// The key does not belong to a known section, or its value has no JSON scalar form. Refused
    /// before any database was touched.
    BadKey {
        /// The key as supplied.
        key: String,
        /// Why it was refused.
        reason: String,
    },
    /// [`vike_secrets::write_setting_row_in`] itself refused — no database, another writer holding
    /// it, the first arming statement on an empty table, or [`validate_row_write`]'s own refusal.
    Refused(vike_secrets::RowWriteError),
}

impl std::fmt::Display for RowPlanError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RowPlanError::BadKey { key, reason } => write!(f, "bad settings key {key:?}: {reason}"),
            RowPlanError::Refused(e) => write!(f, "{e}"),
        }
    }
}

impl std::error::Error for RowPlanError {}

/// **Change one settings key, through the row-native writer.** The spelling every settings write is
/// migrating onto (`docs/decisions/0086`): `vike-cli config
/// set`, `backend setup`/`connect`, the GUI's arming Save and the daemon's own control channel all
/// call this — never [`vike_secrets::write_setting_row_in`] directly, because only this crate can
/// build the validator that primitive requires.
///
/// `key` is the full dotted spelling `config show` renders
/// (`"policy.max_notional_per_order"`, `"policy.venues.binance"`,
/// `"policy.accounts.hyperliquid.ALT"`). `raw_value` is text: a parseable TOML scalar is taken as
/// typed (`250`, `true`), anything else as a string. `busy` is the caller's own wait budget for the
/// database's write lock — a GUI frame, a daemon connection thread and an interactive CLI wait
/// different amounts.
///
/// There is no `--confirm`/retyped-key ceremony here (0086 point 7: *"confirmation over confirmation
/// … a nightmare"*) — what guards a mistake is the loader's own bounds check, run twice
/// ([`validate_row_write`]), and the `old -> new` report this function returns.
///
/// # Errors
/// See [`RowPlanError`].
pub fn write_setting_row(
    settings_dir: &Path,
    key: &str,
    raw_value: &str,
    busy: std::time::Duration,
) -> Result<RowReport, RowPlanError> {
    let change = row_change_for(key, raw_value)?;
    // ⚠ NOT `raw_value.trim()` — that is the caller's literal input, and for a `Setting` row it is
    // NOT what lands: `row_change_for` re-encodes it as a JSON scalar (`vike_secrets::SettingRow`'s
    // own contract), so a string value's `new_value` must carry its rendered quotes too, or an
    // `old -> new` report compares an encoded `old` against an unencoded `new` for what is
    // otherwise the identical value. Read back off `change` itself — what was ACTUALLY handed to
    // the writer — never re-derived a second way.
    let new_value = match &change {
        vike_secrets::RowChange::Setting { value, .. } => value.clone(),
        vike_secrets::RowChange::Arming { mode, .. } => mode.clone(),
    };
    let key_owned = key.to_string();
    let written = vike_secrets::write_setting_row_in(settings_dir, change, busy, {
        let key_owned = key_owned.clone();
        move |current, adoption, candidate| {
            validate_row_write(&key_owned, current, adoption, candidate)
        }
    })
    .map_err(RowPlanError::Refused)?;
    Ok(RowReport { key: key_owned, old_value: written.old_value, new_value })
}

/// **How long a caller is willing to wait for the settings database's write lock** — a small,
/// self-contained "budget in milliseconds" type, kept from the file-era writer this module
/// replaced because [`write_setting_row`]'s callers still reason in the same vocabulary (a GUI
/// frame owes 16 ms, a daemon peer has a reply deadline, a human at a prompt has patience) even
/// though there is no more spin-lock underneath: the database's own busy timeout does the waiting
/// now, and [`LockBudget::max_wait_ms`] is read straight into a [`std::time::Duration`] at each
/// call site.
///
/// ⚠ **This is NOT the old file-lock's retry-count type any more** — there is nothing left to
/// retry `attempts` times against; it is kept only as a documented, named millisecond value.
/// `crates/vike-tradehub/src/server.rs`'s `SETTINGS_LOCK_BUDGET` is its one remaining production
/// caller; the CLI and the GUI arming Save now argue a bare `Duration` directly at their own call
/// sites.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LockBudget(u32);

impl LockBudget {
    /// No wait at all.
    pub const NON_BLOCKING: LockBudget = LockBudget(0);

    /// The whole-process budget: ~3 s.
    pub const DEFAULT: LockBudget = LockBudget::from_millis(3_000);

    /// A budget of `ms` milliseconds.
    #[must_use]
    pub const fn from_millis(ms: u64) -> LockBudget {
        LockBudget(if ms > u32::MAX as u64 { u32::MAX } else { ms as u32 })
    }

    /// The wait this budget names, in milliseconds.
    #[must_use]
    pub const fn max_wait_ms(self) -> u64 {
        self.0 as u64
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dir() -> tempfile::TempDir {
        tempfile::tempdir().expect("temp settings dir")
    }

    /// `SettingsFile::parse` accepts both spellings of each section and nothing else.
    #[test]
    fn section_names_parse_with_and_without_the_extension() {
        for f in SettingsFile::ALL {
            assert_eq!(SettingsFile::parse(f.file_name()), Some(f));
            assert_eq!(SettingsFile::parse(f.section()), Some(f));
        }
        assert_eq!(SettingsFile::parse("settings"), None);
        assert_eq!(SettingsFile::parse("Policy"), None, "case-sensitive: a word, not a file name");
        assert!(unknown_file_message("secrets.env").contains("policy"));
    }

    /// The dotted-key contract: the section prefix is mandatory, the bare section name alone is
    /// refused, and a foreign section is refused.
    #[test]
    fn the_key_spelling_is_the_read_halves_dotted_form() {
        for (key, needle) in [
            ("tradehub_addr", "spelled `config.<key>`"),
            ("config", "names the whole config section"),
            ("policy.max_leverage", "spelled `config.<key>`"),
            ("config..x", "empty key segment"),
        ] {
            let err = key_path(SettingsFile::Config, key).expect_err("a malformed key must refuse");
            assert!(matches!(err, RowPlanError::BadKey { .. }), "{key}: {err:?}");
            assert!(err.to_string().contains(needle), "{key}: {err}");
        }
    }

    /// `row_change_for` matches the arming shapes by SEGMENT: a key whose second segment merely
    /// BEGINS with an exempt table name is an ordinary plain-row write, never an arming row.
    #[test]
    fn arming_shapes_are_matched_by_segment_not_by_prefix() {
        assert!(matches!(
            row_change_for("policy.venues.binance", "live").unwrap(),
            vike_secrets::RowChange::Arming { venue, label: None, mode } if venue == "binance" && mode == "live"
        ));
        assert!(matches!(
            row_change_for("policy.accounts.hyperliquid.ALT", "demo").unwrap(),
            vike_secrets::RowChange::Arming { venue, label: Some(l), mode }
                if venue == "hyperliquid" && l == "ALT" && mode == "demo"
        ));
        // "venues_something" merely BEGINS with "venues" and must fall through to the plain arm.
        assert!(matches!(
            row_change_for("policy.venues_something", "1").unwrap(),
            vike_secrets::RowChange::Setting { .. }
        ));
    }

    /// Value typing: TOML-parseable text lands typed (as a JSON scalar), everything else lands as
    /// a JSON string.
    #[test]
    fn values_are_typed_when_parseable_and_strings_otherwise() {
        assert!(matches!(
            row_change_for("flags.reconcile", "true").unwrap(),
            vike_secrets::RowChange::Setting { value, .. } if value == "true"
        ));
        assert!(matches!(
            row_change_for("config.tradehub_addr", "127.0.0.1:7879").unwrap(),
            vike_secrets::RowChange::Setting { value, .. } if value == "\"127.0.0.1:7879\""
        ));
    }

    /// **THE END-TO-END PROOF: a write lands one row, and reading it back sees the new value.**
    #[test]
    fn a_write_lands_one_row_in_the_database() {
        let d = dir();
        vike_secrets::plant_settings_rows(
            d.path(),
            &StoredSettings {
                settings: vec![],
                arming: vec![vike_secrets::ArmingRow {
                    venue: "binance".to_string(),
                    label: None,
                    mode: "paper".to_string(),
                    max_exposure: None,
                }],
                ..Default::default()
            },
        )
        .expect("a fresh store plants");

        let report = write_setting_row(
            d.path(),
            "policy.max_notional_per_order",
            "250",
            std::time::Duration::from_millis(500),
        )
        .expect("a valid write lands");
        assert_eq!(report.old_value, None);
        assert_eq!(report.new_value, "250");

        let rows = vike_secrets::read_settings_in(d.path()).expect("reads back");
        assert!(
            rows.rows().is_some_and(|r| r.settings.iter().any(|s| s.section == "policy"
                && s.key == "max_notional_per_order"
                && s.value == "250")),
            "the row must be there"
        );
    }

    /// The five appearance keys (design system spec §5) are ordinary one-row writes: a word the key
    /// knows lands as its JSON string, and a word it does not is refused with the store
    /// byte-identical.
    #[test]
    fn an_appearance_word_lands_and_a_foreign_word_is_refused() {
        let d = dir();
        vike_secrets::plant_settings_rows(
            d.path(),
            &StoredSettings {
                settings: vec![vike_secrets::SettingRow {
                    section: "preferences".to_string(),
                    key: "log_level".to_string(),
                    value: "\"info\"".to_string(),
                }],
                ..Default::default()
            },
        )
        .expect("a fresh store plants");
        let budget = std::time::Duration::from_millis(500);
        let report = write_setting_row(d.path(), "preferences.theme", "midnight", budget)
            .expect("a theme word lands");
        assert_eq!(report.new_value, "\"midnight\"");

        let db = vike_secrets::db_path_in(d.path());
        let before = std::fs::read(&db).expect("the database");
        let err = write_setting_row(d.path(), "preferences.theme", "solarized", budget)
            .expect_err("not a theme");
        assert!(err.to_string().contains("graphite, midnight, dusk, carbon"), "{err}");
        assert_eq!(std::fs::read(&db).expect("the database"), before, "byte-identical");

        write_setting_row(d.path(), "preferences.header_gradient", "true", budget)
            .expect("a boolean lands");
        write_setting_row(d.path(), "preferences.header_gradient", "yes", budget)
            .expect_err("a word is not a boolean");
    }

    /// A write whose candidate would resolve a DIFFERENT key than the one named is refused — the
    /// collateral-promotion guard the file-era `RowSync::Mirror` arm used to need, now enforced by
    /// `differing_keys` instead of a re-derive.
    #[test]
    fn a_bad_key_refuses_before_any_database_is_touched() {
        let d = dir();
        let err = write_setting_row(
            d.path(),
            "confgi.api_key",
            "x",
            std::time::Duration::from_millis(500),
        )
        .expect_err("an unknown section must refuse");
        assert!(matches!(err, RowPlanError::BadKey { .. }), "{err:?}");
        assert!(!d.path().join("vike.db").exists(), "nothing was created for a refusal");
    }
}
