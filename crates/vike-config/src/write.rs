//! **The settings write PLANNER** (`docs/decisions/0086`: settings live only in the database, and
//! a write is one row) — turn a caller-facing dotted key and raw value into the one-row
//! [`vike_secrets::RowChange`] change [`vike_secrets::write_setting_row_in`] executes, and build
//! the validator that primitive requires. `write_setting_row` is the ONE spelling every settings
//! write in this tree is migrating onto: `vike-cli config set`, `backend setup`/`connect`, the
//! GUI's settings Saves and the daemon's own control channel all call it — never
//! [`vike_secrets::write_setting_row_in`] directly, because only this crate (layer 20) can resolve
//! rows into a typed [`crate::config::Config`]/[`crate::policy::Policy`]/… through
//! [`crate::load_with_source`] and [`crate::drift::differing_keys`], both of which
//! [`vike_secrets`] (layer 15) must not depend upward to reach.
//!
//! # One store, four sections
//!
//! `<project>/settings/db/vike.db` is the only settings store (0086 point 1), and the write lock
//! is the store's own `BEGIN IMMEDIATE` (point 4). [`SettingsSection`] names the SECTION WORD a
//! dotted key's first segment must match (`"policy"` / `"config"` / `"preferences"` / `"flags"`)
//! — the same four words `vike-cli config show` groups its rows under, and the same four
//! [`crate::config::Config`]/[`crate::policy::Policy`]/[`crate::preferences::Preferences`]/
//! [`crate::flags::Flags`] types [`fn@crate::load`] resolves.
//!
//! # What a write actually does now
//!
//! 1. **[`row_change_for`] resolves the key grammar**, matched by SEGMENT (never by string
//!    prefix — the deleted `is_policy_plane_key`'s tombstone below says why that matters): every
//!    key is one plain `setting` row keyed on the section word
//!    ([`vike_secrets::RowChange::Setting`]), its value typed the same way this module always typed
//!    a raw string (a parseable TOML scalar lands typed, anything else lands as a string) and
//!    rendered through the one JSON-scalar encoder, [`crate::mirror::json_scalar`]. An account's
//!    tier is no setting: it is its `account` row, written by the account verbs.
//! 2. **[`validate_row_write`] is the validator [`vike_secrets::write_setting_row_in`] calls inside
//!    its own open transaction**, before a row is touched: the CURRENT store must already boot
//!    clean if it claims a seal (a write must not re-bless an erased ceiling), and the CANDIDATE —
//!    current rows plus this one change — must boot clean too and [`crate::drift::differing_keys`]
//!    between the two resolutions may name only the written key.
//! 3. **The primitive does the rest** inside one `BEGIN IMMEDIATE`: it upserts the one row, moves
//!    the integrity seal by the same delta, and commits — or rolls back on any refusal, leaving the database BYTE-IDENTICAL. See
//!    `vike_secrets::write_setting_row_in`'s own doc for the transaction.
//!
//! Between steps 1 and 2, before the database is opened at all, [`refuse_credential_key`] refuses a
//! credential-shaped key (next section).
//!
//! # A credential-shaped key is refused HERE, for every surface
//!
//! A credential never travels as a settings row, whichever surface asks
//! (`docs/decisions/0036-credentials-are-read-only-from-the-cli-and-the-mcp-surface.md`: a credential
//! has ONE narrow writer, and it is not this one). The refusal used to live only in `vike-cli`
//! (`crates/vike-cli/src/cmd/settings_write.rs`'s `refuse_a_secret_key`), while
//! `docs/decisions/0086-settings-live-only-in-the-database.md` point 6 made the daemon's control
//! channel a second writer with no such fence. Today that gap changes nothing: no settings field is
//! credential-shaped, so [`validate_row_write`] already refuses every such key as one the loader
//! does not know. The fence sits HERE anyway, in the one planner every writer calls, so the rule has
//! one definition and a new writer inherits it instead of having to remember it.
//! [`refuse_credential_key`] carries the predicate and the residual.
//!
//! # No retype confirm, for any key (0086 point 7)
//!
//! `typed_confirm_reason`, `CONFIRMED_FLAG_KEYS`, `requires_typed_confirm` and the `--confirm` flag
//! are DELETED, not merely unused: the owner's ruling is that a retype
//! stops nothing a bounds check and an *old -> new* report do not. What guards a mistake is
//! [`validate_row_write`]'s own two checks,
//! run on every write, and the caller's *old -> new* report — never a ceremony the operator or an
//! agent can satisfy by pressing return.
//!
//! `is_policy_plane_key` (*which section a key's write belongs to*) is DELETED too. ⚠ The
//! client-side ceremonies that keyed on it went under the same point 7 — the GUI Backend-settings
//! editor's Save gate (`crates/vike-app-core/src/ui/tool_views/backend_settings.rs`'s
//! `can_save_fields`, so every row there saves on one click) and the `trade` REPL's retype prompt
//! (its tombstone is `crates/vike-cli/src/cmd/trade.rs`'s `typed_key_confirm`) — and the `mcp`
//! server's UNATTENDED gate (`crates/vike-cli/src/cmd/mcp/node_writes.rs`'s `unattended_refusal`)
//! refuses EVERY setting in a session nobody attends (decision 0040), so it needs no section rule
//! at all. The predicate's tombstone is below.
//!
//! # Former names, for the citations that still use them
//!
//! Earlier drafts of this design (`docs/decisions/0028`, `0055`, `0057` and the node-onboarding
//! spec) call this module's write entry point `set_setting` or `set_setting_within` — this file's
//! actual function is [`write_setting_row`], never spelled either of those two ways. The same
//! records and the settings-store schema spec call [`SettingsSection`] by its earlier name,
//! `SettingsFile`, and [`RowPlanError`] by its predecessor's, `SettingsWriteError` (whose
//! `Stranded` shape has no successor: a refused row write leaves the database byte-identical).
//! The old whole-file writer also carried a test named
//! `a_broken_current_file_is_refused_not_clobbered`, which asserted the never-clobber property
//! `crate::mirror::seal_refusal` and [`vike_secrets::write_setting_row_in`]'s own
//! byte-identical-on-refusal guarantee now carry instead; that test does not exist in this file
//! any more.

use std::path::Path;

use vike_secrets::{Adoption, StoredSettings};

use crate::layers::CliOverrides;
use crate::source::StoreLayer;

/// One of the four settings SECTIONS, by the word a dotted key's first segment must equal
/// (`"policy"` / `"config"` / `"preferences"` / `"flags"`) — the same grouping
/// `vike-cli config show` renders.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SettingsSection {
    /// The hard ceilings ([`crate::Policy`]).
    Policy,
    /// Deployment settings ([`crate::Config`]).
    Config,
    /// Taste and tuning ([`crate::Preferences`]).
    Preferences,
    /// Operator toggles ([`crate::Flags`]).
    Flags,
}

impl SettingsSection {
    /// Every section, in the loader's application order.
    pub const ALL: [SettingsSection; 4] = [
        SettingsSection::Policy,
        SettingsSection::Config,
        SettingsSection::Preferences,
        SettingsSection::Flags,
    ];

    /// Parse a caller-supplied section word (`"policy"`), case-sensitive — exactly what
    /// [`Self::section`] returns, and nothing else. `None` for anything else.
    pub fn parse(name: &str) -> Option<SettingsSection> {
        SettingsSection::ALL.into_iter().find(|s| s.section() == name)
    }

    /// The dotted-key SECTION word this section's keys are spelled under (`"policy"` for
    /// `policy.max_notional_per_order`) — the first segment of every `vike-cli config show` /
    /// `WireSettingsRow` key, which is also the spelling [`row_change_for`] demands.
    pub fn section(self) -> &'static str {
        match self {
            SettingsSection::Policy => "policy",
            SettingsSection::Config => "config",
            SettingsSection::Preferences => "preferences",
            SettingsSection::Flags => "flags",
        }
    }

    /// **Which section a dotted KEY belongs to, decided from its SECTION WORD** — the first
    /// segment of `policy.max_notional_per_order`. `None` when the first segment names none of the
    /// four.
    pub fn of_key(key: &str) -> Option<SettingsSection> {
        let head = key.split('.').next()?;
        SettingsSection::ALL.into_iter().find(|s| s.section() == head)
    }
}

// ⚠ `is_policy_plane_key` is DELETED: "is `key` in the POLICY plane?", decided from the key's
// SECTION WORD (`SettingsSection::of_key`); decision 0040's ruling left it no caller. The name
// stays in this comment because records and comments cite it.

/// The refusal for a section word that names none of the four settings sections — one message,
/// naming all four in the dotted spelling `vike-cli config show` renders them in, so a caller's
/// typo gets the full menu instead of a guess.
pub fn unknown_section_message(name: &str) -> String {
    format!(
        "unknown settings section {name:?} — every key starts with one of the four `config \
         show` groups its rows under: policy.<key>, config.<key>, preferences.<key> or \
         flags.<key>"
    )
}

/// Split the full dotted key into its in-section path, refusing a spelling that does not fit
/// `section`: the first segment must be the section's word, at least one segment must follow, and
/// no segment may be empty.
fn key_path(section: SettingsSection, dotted_key: &str) -> Result<Vec<&str>, RowPlanError> {
    let bad = |reason: String| RowPlanError::BadKey { key: dotted_key.to_string(), reason };
    let mut segs: Vec<&str> = dotted_key.split('.').collect();
    if segs.iter().any(|s| s.trim().is_empty()) {
        return Err(bad("empty key segment".to_string()));
    }
    if segs[0] != section.section() {
        return Err(bad(format!(
            "a {} key is spelled `{}.<key>` (the same dotted form `config show` renders), got \
             first segment `{}`",
            section.section(),
            section.section(),
            segs[0]
        )));
    }
    segs.remove(0);
    if segs.is_empty() {
        return Err(bad(format!(
            "it names the whole {} section — a write sets one key inside it",
            section.section()
        )));
    }
    Ok(segs)
}

/// One key, resolved into the [`vike_secrets::RowChange`] it writes: a plain `setting` row keyed
/// on the section word, the section matched by SEGMENT and never by a string-prefix test.
fn row_change_for(key: &str, raw_value: &str) -> Result<vike_secrets::RowChange, RowPlanError> {
    let section = SettingsSection::of_key(key).ok_or_else(|| RowPlanError::BadKey {
        key: key.to_string(),
        reason: unknown_section_message(key.split('.').next().unwrap_or(key)),
    })?;
    let segs = key_path(section, key)?;

    // The value is stored as one JSON SCALAR
    // ([`vike_secrets::SettingRow::value`]'s own contract) — typed (a parseable TOML scalar is
    // taken as typed, anything else is a string), then rendered through the one JSON-scalar
    // encoder.
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
        section: section.section().to_string(),
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
///    only `key` itself.
fn validate_row_write(
    key: &str,
    current: &StoredSettings,
    current_adoption: Option<&Adoption>,
    candidate: &StoredSettings,
) -> Result<(), String> {
    let cli = CliOverrides::default();

    let resolve = |rows: &StoredSettings, adoption: &Adoption| {
        crate::load_with_source(None, StoreLayer::Rows { rows, adopted: Some(adoption) }, &cli)
    };
    let synthetic = |rows: &StoredSettings| Adoption {
        adopted_at: String::new(),
        tool_version: String::new(),
        files_present: String::new(),
        setting_rows: rows.settings.len(),
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
        if d.key != key {
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
/// BYTE-IDENTICAL on a refusal (`vike_secrets::write_setting_row_in`'s own doc), so there is no
/// `Stranded` shape to distinguish.
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
    /// it, or [`validate_row_write`]'s own refusal.
    Refused(vike_secrets::RowWriteError),
    /// The key has a credential-shaped name ([`refuse_credential_key`]). Refused before any
    /// database was touched. The VALUE is not carried, so no message built from this variant can
    /// repeat it.
    CredentialKey {
        /// The key as supplied.
        key: String,
    },
}

impl std::fmt::Display for RowPlanError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RowPlanError::BadKey { key, reason } => write!(f, "bad settings key {key:?}: {reason}"),
            RowPlanError::Refused(e) => write!(f, "{e}"),
            // ⚠ **The message answers BOTH readings of a match.** The predicate catches any leaf
            // ending `_KEY`/`_USER`/`_LOGIN`/…, so a plain typo (`config.no_such_key`) lands here as
            // well, and an operator who mistyped a settings key must be pointed at the list of
            // settings keys, not into the credential store. Moved here from `vike-cli` with the
            // refusal; each surface prints it whole.
            RowPlanError::CredentialKey { key } => write!(
                f,
                "`{key}` has a credential-shaped name and no settings write will take one.\n  \
                 If you meant a CREDENTIAL: they live in the store, not in the settings database \
                 — `vike-cli secrets set <KEY>` is the writer, it takes the value on stdin rather \
                 than on the command line, and `vike-cli secrets path` prints which file it \
                 opens.\n  \
                 If you meant a SETTING: this is not a key the loader knows (no settings field is \
                 credential-shaped) — `vike-cli config show` prints every settings key, in the \
                 spelling a write takes."
            ),
        }
    }
}

impl std::error::Error for RowPlanError {}

/// **Refuse a credential-shaped settings key — the ONE definition every settings writer shares.**
/// [`write_setting_row`] calls it before it opens the database, so the daemon's control channel,
/// the GUI and `vike-cli` all refuse the same keys, in the same words
/// ([`RowPlanError::CredentialKey`]'s `Display`). `vike-cli` also calls it EARLY, before it resolves
/// a settings directory, because its refusal of a verb's own input writes no change-journal row
/// (`crates/vike-cli/src/cmd/settings_write.rs`'s module doc, decision 2).
///
/// The predicate is [`crate::redact::is_secret_key`], the REDACTION predicate, applied to the dotted
/// key's leaf. It is reused rather than forked, for the reason `crates/vike-config/src/redact.rs`'s
/// module doc gives: a second copy of a security table is the one duplication this tree cannot
/// afford.
///
/// ⚠ **Residual: the predicate over-matches.** `_USER` and `_LOGIN` name IDENTIFIERS rather than
/// secrets, so a future `config.db_user` would be a legitimate settings field no surface could
/// write. Unreachable today (no settings field is credential-shaped); the day one is wanted, this
/// refusal is what has to be re-decided.
///
/// # Errors
/// [`RowPlanError::CredentialKey`] for a credential-shaped key.
pub fn refuse_credential_key(key: &str) -> Result<(), RowPlanError> {
    if !crate::redact::is_secret_key(key) {
        return Ok(());
    }
    Err(RowPlanError::CredentialKey { key: key.to_string() })
}

/// **Change one settings key, through the row-native writer.** The spelling every settings write is
/// migrating onto (`docs/decisions/0086`): `vike-cli config
/// set`, `backend setup`/`connect`, the GUI's settings Saves and the daemon's own control channel
/// all call this — never [`vike_secrets::write_setting_row_in`] directly, because only this crate
/// can build the validator that primitive requires.
///
/// `key` is the full dotted spelling `config show` renders
/// (`"policy.max_notional_per_order"`, `"flags.reconcile"`). `raw_value` is text: a parseable TOML scalar is taken as
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
    // After the key grammar (an unknown section stays a `BadKey`) and BEFORE the database is
    // opened: a credential-shaped key costs no lock and leaves the store byte-identical.
    refuse_credential_key(key)?;
    // ⚠ NOT `raw_value.trim()` — that is the caller's literal input, and for a `Setting` row it is
    // NOT what lands: `row_change_for` re-encodes it as a JSON scalar (`vike_secrets::SettingRow`'s
    // own contract), so a string value's `new_value` must carry its rendered quotes too, or an
    // `old -> new` report compares an encoded `old` against an unencoded `new` for what is
    // otherwise the identical value. Read back off `change` itself — what was ACTUALLY handed to
    // the writer — never re-derived a second way.
    let vike_secrets::RowChange::Setting { value: new_value, .. } = &change;
    let new_value = new_value.clone();
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
/// self-contained "budget in milliseconds" type, because [`write_setting_row`]'s callers reason in
/// that vocabulary (a GUI frame owes 16 ms, a daemon peer has a reply deadline, a human at a prompt
/// has patience): the database's own busy timeout does the waiting, and
/// [`LockBudget::max_wait_ms`] is read straight into a [`std::time::Duration`] at each call site.
///
/// ⚠ **This is NOT a retry count** — it is only a documented, named millisecond value.
/// `crates/vike-tradehub/src/server/settings.rs`'s `SETTINGS_LOCK_BUDGET` is its one remaining production
/// caller; the CLI and the GUI argue a bare `Duration` directly at their own call sites.
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

#[path = "write_tests.rs"]
#[cfg(test)]
mod write_tests;
