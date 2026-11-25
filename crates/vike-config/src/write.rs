//! **The settings write PLANNER** (`docs/decisions/0086`: settings live only in the database, and
//! a write is one row) — turn a caller-facing dotted key and raw value into the one-row
//! [`vike_secrets::RowChange`] change [`vike_secrets::write_setting_row_in`] executes, and build
//! the validator that primitive requires. `write_setting_row` is the ONE spelling every settings
//! write in this tree is migrating onto: `vike-cli config set`, `backend mint-missing`/`connect`, the
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
//! credential-shaped key (a later section).
//!
//! # Removing a row: [`unset_setting_row`] (decision 0161)
//!
//! `vike-cli config unset <key>` is the one verb that takes a `setting` row away, and it exists for
//! one reason: a key that is retired, or a row the loader refuses, makes [`crate::mirror::apply_rows`]
//! mark the whole store (`vike-tradehub`, `trade`, `mcp` and `config check` then refuse it), and
//! nothing could remove the row — a hand `DELETE` changes the count the seal holds, and the backup
//! still holds the row. It plans a [`vike_secrets::RowChange::Remove`] and builds its OWN
//! validator, [`validate_row_unset`], because [`validate_row_write`] cannot serve it:
//!
//! * [`validate_row_write`] demands that the CURRENT store boot clean, which is exactly what a store
//!   that needs this verb does not do. A removal is the way back from that state, so it asks the
//!   narrower question: **is the seal's row count already sound?** (the one home of that rule is
//!   [`crate::mirror::adoption_integrity`]). A count that is off is the erase detector's finding, and
//!   removing a row from such a store would only move the count and hide it — so that is refused,
//!   and the cure stays the backup. Every other store is allowed to lose the row it names.
//! * When the current store DOES boot clean, the removal gets the full check a write gets: the store
//!   without the row must boot clean too, and no key but the named one may resolve differently.
//! * It never echoes a value (a leftover key's value is not ours to repeat), takes a key in one of
//!   the four sections only, and refuses a credential-shaped key outright, for the reason
//!   [`refuse_credential_key`] gives. It touches no table but `setting` and the seal.
//!
//! The row count and the row go together or not at all — see [`vike_secrets::RowChange::Remove`].
//! Nothing in the daemon reaches this: there is no wire arm for it, so a running node cannot be
//! asked to unset a key, and a removal is never hot (`crates/vike-tradehub/src/hot_reload.rs`'s
//! `classify` is only ever asked about a wire `SetSetting`). The running process keeps the value it
//! booted with, and the next boot reads the default.
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
//! editor's Save gate (`crates/vike-app-core/src/ui/windows/backend/settings/mod.rs`'s
//! `can_save_fields`, so every row there saves on one click) and the `trade` REPL's retype prompt
//! (its tombstone is `crates/vike-cli/src/cmd/trade.rs`'s `typed_key_confirm`) — and the `mcp`
//! server's UNATTENDED gate (`crates/vike-mcp/src/node_writes.rs`'s `unattended_refusal`)
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
use crate::refusals::SettingsWriteRefusal;
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
    /// `SettingsShowRowWire` key, which is also the spelling [`row_change_for`] demands.
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

// ⚠ `unknown_section_message` is DELETED: its sentence is `SettingsWriteRefusal::UnknownSection`'s
// now (`crate::refusals`), one message naming all four sections in the dotted spelling `vike-cli
// config show` renders them in, so a caller's typo gets the full menu instead of a guess. The name
// stays in this comment because records and comments cite it.

/// Split the full dotted key into its in-section path, refusing a spelling that does not fit
/// `section`: the first segment must be the section's word, at least one segment must follow, and
/// no segment may be empty.
fn key_path(section: SettingsSection, dotted_key: &str) -> Result<Vec<&str>, RowPlanError> {
    let key = dotted_key.to_string();
    let mut segments: Vec<&str> = dotted_key.split('.').collect();
    if segments.iter().any(|s| s.trim().is_empty()) {
        return Err(RowPlanError::BadKey(SettingsWriteRefusal::EmptySegment { key }));
    }
    if segments[0] != section.section() {
        return Err(RowPlanError::BadKey(SettingsWriteRefusal::WrongFirstSegment {
            key,
            section: section.section(),
            first: segments[0].to_string(),
        }));
    }
    segments.remove(0);
    if segments.is_empty() {
        return Err(RowPlanError::BadKey(SettingsWriteRefusal::WholeSection {
            key,
            section: section.section(),
        }));
    }
    Ok(segments)
}

/// **Where in the `setting` table a dotted key lives** — its section and the key inside it —
/// matched by SEGMENT and never by a string-prefix test. The one resolution both [`row_change_for`]
/// (a write) and [`unset_setting_row`] (a removal) start from, so the two can never disagree about
/// which row a key names.
fn row_address(key: &str) -> Result<(SettingsSection, String), RowPlanError> {
    let section = SettingsSection::of_key(key).ok_or_else(|| {
        RowPlanError::BadKey(SettingsWriteRefusal::UnknownSection {
            key: key.to_string(),
            name: key.split('.').next().unwrap_or(key).to_string(),
        })
    })?;
    let segments = key_path(section, key)?;
    Ok((section, segments.join(".")))
}

/// One key, resolved into the [`vike_secrets::RowChange`] it writes: a plain `setting` row keyed
/// on the section word, the section matched by SEGMENT and never by a string-prefix test.
fn row_change_for(key: &str, raw_value: &str) -> Result<vike_secrets::RowChange, RowPlanError> {
    let (section, row_key) = row_address(key)?;

    // The value is stored as one JSON SCALAR
    // ([`vike_secrets::SettingRow::value`]'s own contract) — typed (a parseable TOML scalar is
    // taken as typed, anything else is a string), then rendered through the one JSON-scalar
    // encoder.
    let trimmed = raw_value.trim();
    let typed: toml::Value =
        trimmed.parse().unwrap_or_else(|_| toml::Value::String(trimmed.to_string()));
    let value = crate::mirror::json_scalar(&typed).ok_or_else(|| {
        RowPlanError::BadKey(SettingsWriteRefusal::NotAScalar {
            key: key.to_string(),
            shape: crate::mirror::unrenderable_shape(&typed),
        })
    })?;
    Ok(vike_secrets::RowChange::Setting {
        section: section.section().to_string(),
        key: row_key,
        value,
    })
}

/// Resolve `rows` the way a daemon would — no CLI layer, the seal as given — for the validators.
fn resolve_rows(
    rows: &StoredSettings,
    adoption: &Adoption,
) -> Result<crate::Settings, crate::ConfigError> {
    crate::load_with_source(
        None,
        StoreLayer::Rows { rows, adopted: Some(adoption) },
        &CliOverrides::default(),
    )
}

/// **The seal a store holding exactly `rows` would carry if its count were sound** — the stand-in
/// for "the seal this write is ABOUT to leave behind", which the validators resolve a candidate
/// under (the real one is written by the primitive after the validator has answered).
///
/// It is also the seal every loader test hands `StoreLayer::Rows`: the count is taken from the rows
/// rather than typed, because that is exactly what the writer does
/// (`vike_secrets::write_setting_row_in` counts inside its own transaction), and a fixture that
/// typed it would be measuring a state the writer cannot produce. No loader reads the other cells.
pub fn synthetic_seal(rows: &StoredSettings) -> Adoption {
    Adoption {
        adopted_at: String::new(),
        tool_version: String::new(),
        files_present: String::new(),
        setting_rows: rows.settings.len(),
    }
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
    // Each verdict is its refusal's rendered form, and one that wraps a resolve error carries THAT
    // error's rendered form too, so a person reads both ids.
    let current_unresolved = |e: crate::ConfigError| {
        SettingsWriteRefusal::CurrentRowsUnresolved { error: e.rendered() }.rendered()
    };
    if let Some(adoption) = current_adoption {
        let settings = resolve_rows(current, adoption).map_err(current_unresolved)?;
        if let Some(why) = settings.seal_refusal.or(settings.store_refusal) {
            return Err(SettingsWriteRefusal::CurrentStoreRefused { why }.rendered());
        }
    }

    let current_synth = synthetic_seal(current);
    let current_settings = resolve_rows(current, current_adoption.unwrap_or(&current_synth))
        .map_err(current_unresolved)?;

    let candidate_synth = synthetic_seal(candidate);
    let candidate_settings = resolve_rows(candidate, &candidate_synth)
        .map_err(|e| SettingsWriteRefusal::ValueUnresolved { error: e.rendered() }.rendered())?;
    if let Some(why) =
        candidate_settings.seal_refusal.as_ref().or(candidate_settings.store_refusal.as_ref())
    {
        return Err(SettingsWriteRefusal::WriteWouldNotBoot { why: why.clone() }.rendered());
    }

    for d in crate::drift::differing_keys(&current_settings, &candidate_settings) {
        if d.key != key {
            let crate::DriftedKey { key, before, after } = d;
            let refusal = SettingsWriteRefusal::WriteChangesAnotherKey { key, before, after };
            return Err(refusal.rendered());
        }
    }
    Ok(())
}

/// **The validator [`unset_setting_row`] hands to [`vike_secrets::write_setting_row_in`]** — the
/// narrow one a removal needs, because [`validate_row_write`]'s first check (the current store
/// must already boot clean) is exactly what a store that needs this verb fails.
///
/// 1. **The seal's row COUNT must already be sound**, if the store claims a seal
///    ([`crate::mirror::adoption_integrity`], the one home of that rule). A count that is off means
///    rows arrived or left by some other route; removing a row would only move the count and hide
///    that, so it is refused and the cure stays the backup. A never-sealed store has no count to
///    check.
/// 2. **A store that boots clean gets the full write check** — the store without the row must boot
///    clean too, and no key but `key` may resolve differently — so an unset of a healthy key is
///    held to what a `config set` of it would be.
/// 3. **A store that does NOT boot clean is let through**: that is the repair path. It cannot be
///    made less bootable by losing a row, and the row named is the operator's explicit request;
///    the removal may leave the store still refusing for ANOTHER row, which the caller reports so
///    the verb is run again for that one.
fn validate_row_unset(
    key: &str,
    current: &StoredSettings,
    current_adoption: Option<&Adoption>,
    candidate: &StoredSettings,
) -> Result<(), String> {
    if let Some(adoption) = current_adoption {
        crate::mirror::adoption_integrity(current, adoption)
            .map_err(|e| SettingsWriteRefusal::SealUnsound { error: e.rendered() }.rendered())?;
    }

    let current_synth = synthetic_seal(current);
    let current_settings = resolve_rows(current, current_adoption.unwrap_or(&current_synth))
        .map_err(|e| {
            SettingsWriteRefusal::CurrentRowsUnresolved { error: e.rendered() }.rendered()
        })?;
    if current_settings.seal_refusal.is_some() || current_settings.store_refusal.is_some() {
        return Ok(());
    }

    let candidate_synth = synthetic_seal(candidate);
    let candidate_settings = resolve_rows(candidate, &candidate_synth).map_err(|e| {
        SettingsWriteRefusal::WithoutRowUnresolved { error: e.rendered() }.rendered()
    })?;
    if let Some(why) =
        candidate_settings.seal_refusal.as_ref().or(candidate_settings.store_refusal.as_ref())
    {
        return Err(SettingsWriteRefusal::RemovalWouldNotBoot { why: why.clone() }.rendered());
    }

    for d in crate::drift::differing_keys(&current_settings, &candidate_settings) {
        if d.key != key {
            let crate::DriftedKey { key, before, after } = d;
            let refusal = SettingsWriteRefusal::RemovalChangesAnotherKey { key, before, after };
            return Err(refusal.rendered());
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
    /// before any database was touched. The refusal names the key as supplied.
    BadKey(SettingsWriteRefusal),
    /// [`vike_secrets::write_setting_row_in`] itself refused — no database, another writer holding
    /// it, a removal of a row that is not there ([`vike_secrets::RowWriteError::NoSuchRow`]), or
    /// [`validate_row_write`]'s / [`validate_row_unset`]'s own refusal.
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
        // `BadKey` and `CredentialKey` read `error [<id>]: <text>`, the id a person looks up; a
        // `Refused` is the primitive's words, whose validator verdict already carries its own.
        match self {
            RowPlanError::BadKey(refusal) => f.write_str(&refusal.rendered()),
            RowPlanError::Refused(e) => write!(f, "{e}"),
            // ⚠ **The message answers BOTH readings of a match.** The predicate catches any leaf
            // ending `_KEY`/`_USER`/`_LOGIN`/…, so a plain typo (`config.no_such_key`) lands here as
            // well, and an operator who mistyped a settings key must be pointed at the list of
            // settings keys, not into the credential store. Moved here from `vike-cli` with the
            // refusal; each surface prints it whole.
            RowPlanError::CredentialKey { key } => {
                f.write_str(&SettingsWriteRefusal::CredentialKey { key: key.clone() }.rendered())
            }
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
/// secrets, and the substring rows (`SEED`, `_KEY`, `TOKEN`, …) swallow any leaf containing them, so
/// a future `config.db_user` or `flags.rng_seed` would be a legitimate settings field no surface
/// could write. Unreachable today: no settings field is credential-shaped except the two BOOLEAN
/// flags `flags.allow_withdraw_keys` and `flags.datahub_chart_seed`, which
/// [`crate::redact::NOT_SECRET`] excuses by name (`redact_tests.rs` pins that this list is exactly
/// those two among the settings keys). The day another is wanted, the excuse or this refusal is
/// what has to be re-decided.
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
/// set`, `backend mint-missing`/`connect`, the GUI's settings Saves and the daemon's own control channel
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
    // A retired flag row LOADS and is read by nothing, so only a new write of one is refused here.
    if let Some((_, why)) = key
        .strip_prefix("flags.")
        .and_then(|field| crate::flags::RETIRED_FLAG_ROWS.iter().find(|(f, _)| *f == field))
    {
        return Err(RowPlanError::BadKey(SettingsWriteRefusal::RetiredFlag {
            key: key.to_string(),
            why,
        }));
    }
    // ⚠ NOT `raw_value.trim()` — that is the caller's literal input, and for a `Setting` row it is
    // NOT what lands: `row_change_for` re-encodes it as a JSON scalar (`vike_secrets::SettingRow`'s
    // own contract), so a string value's `new_value` must carry its rendered quotes too, or an
    // `old -> new` report compares an encoded `old` against an unencoded `new` for what is
    // otherwise the identical value. Read back off `change` itself — what was ACTUALLY handed to
    // the writer — never re-derived a second way.
    let new_value = match &change {
        vike_secrets::RowChange::Setting { value, .. } => value.clone(),
        vike_secrets::RowChange::Remove { .. } => {
            unreachable!("`row_change_for` plans only a Setting change")
        }
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

/// **The one key whose "unset" default is not "nothing"**: `policy.link_deadman_grace_ms` serializes to
/// no value when no row sets it (the shape of its per-order sibling), yet an absent row means the
/// connection dead-man is ARMED at [`crate::DEFAULT_LINK_DEADMAN_GRACE_MS`]
/// (`Policy::link_deadman_grace_ms_effective`). A report that said "no value" for it would read, to
/// the operator the dead-man warnings send here, as the switch still being off — the opposite of
/// what happens. `config show` has the same blank DEFAULT cell for it and carries the argument in
/// `provenance::setting_keys`; this is that fact, said where a removal reports.
fn armed_by_default(dotted_key: &str) -> Option<String> {
    if dotted_key != "policy.link_deadman_grace_ms" {
        return None;
    }
    crate::Policy::default().link_deadman_grace_ms_effective().map(|ms| {
        format!("{ms} ms — the connection dead-man is ARMED at this grace while no row is written")
    })
}

/// What the key resolves to once [`unset_setting_row`] has taken its row away.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum KeyAfterRemoval {
    /// A key the loader knows: it is its compiled-in default again. The payload renders that
    /// default the way `config show` does, or is `None` when the default is "unset" — no value at
    /// all, which for a ceiling means NO CAP.
    Default(Option<String>),
    /// A key the loader does not know — a retired, tombstoned or hand-inserted row. Nothing read it
    /// (the loader refused the store over it), so removing it changes no value anything resolves.
    Unknown,
}

/// What [`unset_setting_row`] did.
///
/// ⚠ **It carries no value of the removed row, by design.** A leftover key's value is not ours to
/// repeat — a credential pasted into the wrong key is a plausible first-time mistake — so the
/// report names the key and what it resolves to NOW, never what it held.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RowRemoval {
    /// The full dotted key, as the caller spelled it.
    pub key: String,
    /// What the key resolves to now.
    pub now: KeyAfterRemoval,
    /// The seal's row count before and after — `Some((before, after))` on a sealed store, `None` on
    /// a store that was never sealed (a removal does not seal one).
    pub seal: Option<(usize, usize)>,
    /// `Some(why)` when the store STILL does not boot clean without this row — another refused row,
    /// which the caller names so the verb is run again for it. `None` when it boots clean.
    pub still_refused: Option<String>,
}

/// **Remove ONE settings row, keeping the seal valid in the same transaction** — the cure for a
/// store the loader refuses over a retired or illegal row, and the way a known key goes back to its
/// default. `vike-cli config unset` is its one operator surface (decision 0161).
///
/// `key` is the full dotted spelling `config show` renders (`"policy.max_total_exposure"`,
/// `"flags.reconcile"`) and may name a key the loader no longer knows: that is the point. `busy` is
/// the caller's own wait budget for the database's write lock, as for [`write_setting_row`].
///
/// What it refuses, each before or without touching the store:
/// * a key outside the four sections, or naming a whole section ([`RowPlanError::BadKey`]);
/// * a credential-shaped key ([`refuse_credential_key`]) — the database is the credential store, and
///   no verb that takes a key from the command line may reach one;
/// * a key that has no row ([`vike_secrets::RowWriteError::NoSuchRow`]);
/// * a store whose seal count is already off, and a known key whose removal would leave a healthy
///   store unable to boot ([`validate_row_unset`] says which and why).
///
/// # Errors
/// See [`RowPlanError`].
pub fn unset_setting_row(
    settings_dir: &Path,
    key: &str,
    busy: std::time::Duration,
) -> Result<RowRemoval, RowPlanError> {
    let (section, row_key) = row_address(key)?;
    // After the key grammar and BEFORE the database is opened, exactly as `write_setting_row`: a
    // credential-shaped key costs no lock and leaves the store byte-identical.
    refuse_credential_key(key)?;
    let change =
        vike_secrets::RowChange::Remove { section: section.section().to_string(), key: row_key };
    let mut sealed = false;
    let written = vike_secrets::write_setting_row_in(
        settings_dir,
        change,
        busy,
        |current, adoption, candidate| {
            sealed = adoption.is_some();
            validate_row_unset(key, current, adoption, candidate)
        },
    )
    .map_err(RowPlanError::Refused)?;

    // The report is read back off what COMMITTED: the rows the primitive hands back are the
    // candidate it wrote, so `after` is the count the seal now holds and `before` the row it lost.
    let after = written.rows.settings.len();
    let now = match crate::provenance::setting_keys().into_iter().find(|spec| spec.key == key) {
        Some(spec) => KeyAfterRemoval::Default(
            armed_by_default(&spec.key)
                .or_else(|| crate::provenance::effective_value(&crate::Settings::default(), &spec)),
        ),
        None => KeyAfterRemoval::Unknown,
    };
    // Whether the store loads NOW. The count is sound by construction (the validator refused any
    // other), so the stand-in seal asks the row-level question and nothing else.
    let still_refused = resolve_rows(&written.rows, &synthetic_seal(&written.rows))
        .ok()
        .and_then(|settings| settings.seal_refusal.or(settings.store_refusal));
    Ok(RowRemoval {
        key: key.to_string(),
        now,
        seal: sealed.then_some((after + 1, after)),
        still_refused,
    })
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
