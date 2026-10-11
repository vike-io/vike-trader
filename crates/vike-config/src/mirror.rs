//! **The settings database's rows, applied as the ONLY settings layer** —
//! `docs/decisions/0086`: settings live only in the database, and there is no file underneath the
//! rows to fall back to.
//!
//! ⚠ **One direction only: ROWS TO SETTINGS.** A row is created only by
//! `crate::write::write_setting_row` (the reverse half, `rows_from_files`, is deleted — 0086):
//!
//! * [`section_values`] / [`apply_rows`] — PARSE each stored value on its own, assemble the
//!   section's JSON object out of the results, and deserialize **the existing patch type** from
//!   that object, so a hand-`INSERT`ed typo'd key still gets the `deny_unknown_fields` refusal
//!   BY NAME, every bound stays imported from `vike-model`, and every tombstone keeps its own
//!   message.
//!
//! # ⚠ The value column holds a JSON SCALAR
//!
//! A stored value is parsed by `serde_json` — **the parser decides the type, not this module** —
//! and the patch type is deserialized from the assembled object. Each row's value goes through
//! `serde_json::from_str` ON ITS OWN, before it is placed in the object, so a malformed value fails
//! at its own row, naming that row's key.
//!
//! Two shapes JSON cannot carry and TOML could are refused rather than rendered — see
//! [`unrenderable_shape`], still reached from `crate::write`'s own encoder — and the one shape JSON
//! can carry and TOML could not (`null`) is refused on the way IN, by [`section_values`], because a
//! `null` in an `Option<T>` field deserializes as *"this key is unset"*: the exact silent read this
//! module exists to prevent.
//!
//! # ⚠ Why the round trip has to go through the patch types
//!
//! A `setting` row is a typo'd ROW KEY, not a typo'd column, so the database refuses it in neither
//! direction and no `CHECK` can be written that would. Materialising the section back into a value
//! and deserializing it as [`PolicyPatch`] / [`ConfigPatch`] / [`PreferencesPatch`] / [`FlagsPatch`]
//! is what gives it back.
//!
//! # This crate still never opens the store
//!
//! `crates/vike-config/Cargo.toml` declares its edge to `vike-secrets` with the words *"this crate
//! never opens the store, and must not"*: a handle that reaches the settings rows also reaches the
//! `credential` table, from the crate whose boot disclosure goes into a file shipped with bug
//! reports. So [`StoredSettings`] arrives here as DATA — a PARAMETER, exactly like the environment
//! map — and the BINARY does the opening. `crates/vike-secrets/src/settings.rs` is the other side of
//! that split and carries the table boundary.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::Deserialize;
use vike_secrets::{Adoption, StoredSettings};

use crate::config::ConfigPatch;
use crate::error::{ConfigError, key_from_parse_message};
use crate::flags::FlagsPatch;
use crate::load::Settings;
use crate::policy::PolicyPatch;
use crate::preferences::PreferencesPatch;

/// The four settings SECTIONS this crate resolves — the same vocabulary `vike_secrets::
/// SETTINGS_SECTIONS` uses on the store's side (and the `setting.section` `CHECK`'s).
pub const SETTING_SECTIONS: [&str; 4] = ["policy", "config", "preferences", "flags"];

/// What a refusal raised against a materialised ROW names instead of a path on disk.
///
/// A `ConfigError` carries a path, and these keys came from a table, so the pseudo-path names the
/// store and the section whose schema refused — the section is worth naming, because an operator
/// with twenty `policy` rows needs to know which schema rejected the value.
fn row_source(section: &str) -> PathBuf {
    PathBuf::from(format!("settings database (section `{section}`)"))
}

/// **The JSON rendering of one TOML scalar**, or `None` for a `toml::Value` JSON cannot carry.
///
/// ⚠ **NOT `serde_json::to_string(&value)`, and the difference is MEASURED rather than stylistic.**
/// That call is TOTAL over `toml::Value` and answers for the two shapes JSON has no form for by
/// inventing one: a non-finite float serializes as `null` (which then reads back as *"this key is
/// unset"*, so a ceiling disappears instead of being refused), and a `Datetime` as the
/// `{"$__toml_private_datetime":"…"}` object `toml`'s own serde shim uses to smuggle its type
/// through a self-describing format. This match is EXHAUSTIVE over the enum instead, so a shape
/// with no JSON form has to be classified rather than absorbed by a catch-all.
///
/// The float arm is `serde_json::Number::from_f64`, whose `None` IS the non-finite answer — the
/// finiteness test and the rendering are then one call rather than a check a later edit can drift
/// from. Its rendering is shortest-round-trip (`ryu`), and the workspace pins `serde_json` with
/// `float_roundtrip` so the parse back is correctly rounded: MEASURED bit-exact over 20 000 random
/// finite `f64` bit patterns, with zero disagreements against the TOML round trip it replaces.
///
/// This is `crate::write`'s ONE encoder — every settings write renders its value through this same
/// function, so a row written by `vike-cli config set` and a row this module reads back agree by
/// construction.
pub(crate) fn json_scalar(value: &toml::Value) -> Option<String> {
    match value {
        toml::Value::String(s) => Some(serde_json::Value::String(s.clone()).to_string()),
        toml::Value::Integer(i) => Some(i.to_string()),
        toml::Value::Boolean(b) => Some(b.to_string()),
        toml::Value::Float(f) => serde_json::Number::from_f64(*f).map(|n| n.to_string()),
        toml::Value::Datetime(_) | toml::Value::Array(_) | toml::Value::Table(_) => None,
    }
}

/// How a value [`json_scalar`] refused is NAMED in the refusal — an operator is told which shape of
/// their value the store cannot hold, not merely that something failed.
pub(crate) fn unrenderable_shape(value: &toml::Value) -> &'static str {
    match value {
        toml::Value::Datetime(_) => "a TOML DATETIME",
        toml::Value::Float(_) => "a NON-FINITE float (`inf` / `nan`)",
        _ => "a value with no JSON scalar form",
    }
}

// ---------------------------------------------------------------------------------------------
// Rows -> JSON objects -> the patch types
// ---------------------------------------------------------------------------------------------

/// Why a stored row could not become a value.
///
/// ⚠ **The two answers are DISPOSITIONS, not severities**, and both end in
/// [`Settings::mark_seal_refusal`] rather than a hard `Err` ([`apply_rows`] says why). What
/// differs between the two is the MESSAGE, not the mechanism:
///
/// * an `Unreadable` row is a value in a format no mirror ever wrote (a pre-JSON encoding, most
///   often) — the disclosure it costs is real but the repair is unrelated to the value itself;
/// * an `Illegal` row parsed fine and says something no writer of this tree's ever produces — an
///   unknown key, a broken bound, a `null`, an out-of-range integer — which is evidence of a hand
///   `INSERT`, a botched migration, or a restored backup.
#[derive(Debug)]
pub enum RowRefusal {
    /// The row is not JSON at all, or is JSON `null`/an out-of-range integer. Almost always a value
    /// written before the value column became JSON, or a row nothing in this tree's writers can
    /// produce.
    Unreadable(String),
    /// The row parsed and says something no writer of this tree's ever produces — an unknown key, a
    /// tombstone, a bound violation.
    Illegal(ConfigError),
}

impl From<ConfigError> for RowRefusal {
    fn from(e: ConfigError) -> Self {
        RowRefusal::Illegal(e)
    }
}

/// **Assemble the rows of every section into one JSON OBJECT per section** that has any rows at
/// all, parsing each stored value as its own complete JSON document.
///
/// Every value reaches the object through [`parse_row_value`] and through nothing else, which is
/// what carries the property this path exists for: a row whose value is malformed fails AT ITS OWN
/// ROW, naming that row's key, instead of being placed into the object unparsed and smuggled into
/// the typed model. The refusals it can raise, in the order they are reached:
///
/// * a value that is not one complete JSON document — including a stale TOML rendering that JSON
///   has no form for, and a value carrying trailing text after its scalar;
/// * a JSON `null`, which is the one thing JSON can express and TOML could not, and which
///   `deny_unknown_fields` structurally cannot catch: serde sees a KNOWN field name and an
///   `Option<T>` field reads `null` as `None`, so a key an operator filed would silently resolve to
///   its default;
/// * a dotted key that collides with another row — `a` and `a.b` together, which a TOML document
///   would have been refused for by its parser and which `UNIQUE (section, key)` cannot see.
pub fn section_values(
    store: &StoredSettings,
) -> Result<BTreeMap<&'static str, serde_json::Value>, RowRefusal> {
    let mut by_section: BTreeMap<&'static str, serde_json::Map<String, serde_json::Value>> =
        BTreeMap::new();
    for row in &store.settings {
        let Some(section) = SETTING_SECTIONS.iter().copied().find(|s| *s == row.section) else {
            // A section the four-word vocabulary does not carry. `setting.section`'s own `CHECK`
            // refuses one, so it is unreachable through every write this tree has — a hand
            // `INSERT` against a schema that has been altered is the only way to produce it, and
            // skipping rather than panicking keeps that from taking a daemon down.
            //
            // ⚠ **DECLARED RESIDUAL: such a row is read as nothing and NOTHING NAMES IT**, beyond
            // `config check`'s store finding counting rows without classifying their sections.
            continue;
        };
        let file = row_source(section);
        let value = parse_row_value(&file, &row.key, &row.value)?;
        insert_at(&file, by_section.entry(section).or_default(), &row.key, value)?;
    }

    Ok(by_section.into_iter().map(|(s, o)| (s, serde_json::Value::Object(o))).collect())
}

/// **One stored value, parsed as one complete JSON document.**
///
/// `serde_json::from_str` rejects TRAILING INPUT, and that is load-bearing rather than incidental:
/// it is what keeps a row whose value is `3.0, "max_notional_per_order": 9e9` from setting a
/// SECOND key.
///
/// The message never echoes the value. `serde_json::Error`'s `Display` has no input to echo — it
/// renders `expected value at line 1 column 1`, with no annotated snippet. A credential written
/// into the wrong key is a plausible first-time mistake, and the refusal for it must not be the
/// thing that publishes it.
fn parse_row_value(file: &Path, key: &str, raw: &str) -> Result<serde_json::Value, RowRefusal> {
    let value = serde_json::from_str::<serde_json::Value>(raw).map_err(|e| {
        RowRefusal::Unreadable(format!("{}: `{key}` — {e}. {STALE_FORMAT_HINT}", file.display()))
    })?;
    // ⚠ A JSON integer `serde_json` DEMOTED to a float, because it fits neither `i64` nor `u64`.
    // Without this arm the demotion is silent and lands on a CEILING: measured, a
    // `max_notional_per_order` row of `18446744073709551616` resolves to `Some(1.84e19)`, passes
    // `check_positive` (positive and finite), and leaves the per-order cap effectively absent while
    // `config show` reports it resolved. ILLEGAL rather than unreadable — `json_scalar` renders
    // integers from an `i64` and so can never write one, and no writer this tree has ever did.
    if let Some(n) = value.as_number()
        && n.as_i64().is_none()
        && n.as_u64().is_none()
        && !raw.contains(['.', 'e', 'E'])
    {
        return Err(RowRefusal::Illegal(ConfigError::Parse {
            file: file.to_path_buf(),
            key: Some(key.to_string()),
            message: format!(
                "`{key}` is an INTEGER outside the range this store can hold ({raw}). \
                 `serde_json` reads it as a float, so a ceiling written this way would resolve to \
                 an enormous finite number and pass every positivity check — the operator would \
                 believe they filed a cap and have none. No writer in this tree can produce this: a \
                 hand `INSERT`, a botched migration or a restored backup did."
            ),
        }));
    }
    if value.is_null() {
        return Err(RowRefusal::Illegal(ConfigError::Parse {
            file: file.to_path_buf(),
            key: Some(key.to_string()),
            message: format!(
                "`{key}` is JSON `null`, and a settings row may not be. Every field of every patch \
                 type is an `Option`, so `null` deserializes as \"this key is UNSET\" — a key an \
                 operator believes they filed would silently resolve to its default, and \
                 `deny_unknown_fields` cannot catch it because the FIELD NAME is known. No writer \
                 in this tree can produce this: a hand `INSERT`, a botched migration or a restored \
                 backup did."
            ),
        }));
    }
    Ok(value)
}

/// What a refusal adds when a stored value does not parse — the one sentence that turns *"this row
/// is broken"* into *"this row is in a format the column used to hold"*.
///
/// It is a HINT rather than a detection, deliberately: this module cannot tell a pre-JSON rendering
/// from a typo, and claiming to would be a guess. There is no re-derive to point at (0086), so the
/// hint names the shape and stops —
/// the caller's own message is where the repair (restore `vike.db` from the nightly backup) is
/// stated once, at the level that actually knows whether this row's store is sealed.
const STALE_FORMAT_HINT: &str = "A `setting` row holds ONE JSON SCALAR (`100`, `\"warn\"`, `true`). \
                                 This value is in a format the column no longer holds — most likely \
                                 a TOML rendering from before the value column became JSON.";

/// Place one parsed value into its section's object at a DOTTED key — `rate.max_utilization` into
/// `{"rate":{"max_utilization":…}}` — refusing a collision rather than resolving one.
///
/// ⚠ The collision refusal is the half that is not obvious: `a = 1` beside `a.b = 2` is a shape a
/// TOML document's own parser would have refused, while a hand-built object would silently keep
/// whichever row came last. `UNIQUE (section, key)` on the table cannot see it either — the two
/// rows have different keys. So it is checked here, by name.
///
/// ⚠ A row key that literally CONTAINS a dot is indistinguishable from a nested path. No key in the
/// four patch types is spelled with one.
fn insert_at(
    file: &Path,
    object: &mut serde_json::Map<String, serde_json::Value>,
    dotted: &str,
    value: serde_json::Value,
) -> Result<(), ConfigError> {
    let (parents, leaf) = match dotted.rsplit_once('.') {
        Some((parents, leaf)) => (Some(parents), leaf),
        None => (None, dotted),
    };
    let mut cursor = object;
    if let Some(parents) = parents {
        for segment in parents.split('.') {
            cursor = match cursor
                .entry(segment.to_string())
                .or_insert_with(|| serde_json::Value::Object(serde_json::Map::new()))
            {
                serde_json::Value::Object(next) => next,
                _ => return Err(collision(file, dotted)),
            };
        }
    }
    if cursor.contains_key(leaf) {
        return Err(collision(file, dotted));
    }
    cursor.insert(leaf.to_string(), value);
    Ok(())
}

/// The refusal [`insert_at`] raises: two rows of one section claim the same place in its object.
fn collision(file: &Path, key: &str) -> ConfigError {
    ConfigError::Parse {
        file: file.to_path_buf(),
        key: Some(key.to_string()),
        message: format!(
            "`{key}` collides with another row of this section — a key and a path THROUGH that key \
             cannot both be set. Nothing has been read."
        ),
    }
}

/// **Apply the settings database's rows as the ONLY settings layer.**
///
/// `adoption` is the store's integrity seal, or `None` for a store nothing has ever written a row
/// to (the ordinary state of a fresh box: nothing to check, because there is nothing sealed yet).
/// When it is `Some`, [`adoption_integrity`] runs first — the ERASE detector — and its
/// failure, like every other failure this function meets, is MARKED on
/// [`Settings::seal_refusal`] rather than returned as an `Err`.
///
/// ⚠ **NOTHING here returns `Err`, and that is deliberate.** A refusal propagated through
/// `load_with_source`'s `?` would take `vike-cli config show`, `secrets list` and `--help`
/// down WITH the box those commands exist to diagnose. So a bad seal, an unreadable row or an
/// illegal row are all marks, and the enforcement moves to the surfaces that can act on a ceiling
/// without being the diagnosis: `vike-cli trade`/`mcp`/`config check`, and `vike-tradehub` itself
/// refusing to start on an unsound seal (`docs/decisions/0069`). **By ruling, the only repair for a
/// marked store is restoring `vike.db` from the box's nightly backup — no repair command is built**
/// (`docs/decisions/0086`).
pub fn apply_rows(settings: &mut Settings, store: &StoredSettings, adoption: Option<&Adoption>) {
    if let Some(adoption) = adoption {
        match adoption_integrity(store, adoption) {
            Ok(warnings) => settings.warnings.extend(warnings),
            Err(e) => {
                settings.mark_seal_refusal(e.to_string());
                return;
            }
        }
    }
    let values = match section_values(store) {
        Ok(v) => v,
        Err(RowRefusal::Unreadable(why)) => {
            settings.mark_seal_refusal(
                seal_refusal(format!(
                    "{why} Every value below is a compiled-in default until this is repaired, \
                     which for `max_notional_per_order` means NO CEILING."
                ))
                .to_string(),
            );
            return;
        }
        Err(RowRefusal::Illegal(e)) => {
            settings.mark_seal_refusal(e.to_string());
            return;
        }
    };
    if let Err(e) = apply_section_values(settings, &values) {
        settings.mark_seal_refusal(e.to_string());
    }
}

/// **The seal's own integrity — the ERASE detector, checked whenever a store carries one** (i.e.
/// at least one write has ever landed on it).
///
/// `setting_rows` as sealed must equal the `setting` table's actual count. Every sanctioned write
/// updates it inside the transaction that changes the table, so a mismatch in EITHER direction
/// means rows arrived or left by some other route. This is what makes *uncapped through row loss*
/// unrepresentable. (The seal's `arming_rows` and `venues_declared` columns are dead since decision
/// 0119 — the account's tier lives on its `account` row — and are compared to nothing.)
///
/// ⚠ **A COUNT, deliberately, and not a per-row digest.** A digest would make the store
/// tamper-evident and would also refuse a boot over a hand-edited `preferences.log_file_level`, with
/// no repair command built to fix it (0086 point 2: *"restore from the nightly backup only"*).
/// A count is the largest check that cannot brick a box over a LEGAL value. What that leaves silent
/// is stated rather than hidden: **a value changed IN PLACE to another legal value keeps the count
/// and does not refuse**, and `config show` then attributes it to `db` with full confidence, which
/// is correct and is the problem. It is bounded — the pre-trade ceilings that judge every order live
/// in the run profile's `[risk]` table, so an in-place row edit can move the control-channel cap
/// and cannot silently uncap the order path itself.
///
/// The `Ok` vector carries warnings for the caller to surface; no check raises one today.
pub fn adoption_integrity(
    store: &StoredSettings,
    adoption: &Adoption,
) -> Result<Vec<String>, ConfigError> {
    let settings_now = store.settings.len();
    if settings_now != adoption.setting_rows {
        return Err(seal_refusal(format!(
            "the settings rows have CHANGED since this store was last written to: it was sealed on \
             {} setting row(s) and now carries {settings_now}. Every sanctioned write updates that \
             count in the same transaction that changes the table, so rows arrived or left by some \
             other route — a hand `INSERT`/`DELETE`, a restored backup, or a partially-applied \
             migration. These rows are the ONLY settings layer, so the missing ones are not \
             defaults an operator chose: they are a ceiling that has silently stopped being \
             applied.",
            adoption.setting_rows
        )));
    }
    Ok(Vec::new())
}

/// The shape every [`adoption_integrity`] refusal takes: a `ConfigError` naming the STORE rather
/// than a path on disk, exactly as an illegal row's does — see [`row_source`].
fn seal_refusal(message: String) -> ConfigError {
    ConfigError::Parse {
        file: row_source("policy"),
        key: None,
        // ⚠ **By ruling, there is no repair command** (`docs/decisions/0086` point 2: an unsound
        // store is repaired by restoring `vike.db` from the box's nightly backup, and nothing
        // else). `vike-cli config mirror`/`config adopt --undo` are DELETED, and naming a deleted
        // command as the way out would be a refusal whose remedy does not work.
        message: format!(
            "{message} There is no repair command for this: restore the settings database from \
             the box's nightly backup."
        ),
    }
}

/// The apply itself, kept as its own function so a future second seal-checked entry point cannot
/// drift about what a row MEANS.
///
/// Every section's assembled OBJECT is deserialized as its patch and handed to the same `apply` a
/// caller-supplied value always used, so an unknown key is refused BY NAME, a tombstoned key raises
/// its own message, and every bound is the one `vike-model` exports. The pseudo-file in the error
/// names the STORE and the section rather than a path on disk — see [`row_source`].
fn apply_section_values(
    settings: &mut Settings,
    values: &BTreeMap<&'static str, serde_json::Value>,
) -> Result<(), ConfigError> {
    if let Some(object) = values.get("policy") {
        let file = row_source("policy");
        let patch: PolicyPatch = from_json_object(&file, object)?;
        settings.policy.apply(patch, &file)?;
    }
    if let Some(object) = values.get("config") {
        let file = row_source("config");
        let patch: ConfigPatch = from_json_object(&file, object)?;
        settings.config.apply(patch, &file)?;
    }
    if let Some(object) = values.get("preferences") {
        let file = row_source("preferences");
        let patch: PreferencesPatch = from_json_object(&file, object)?;
        settings.preferences.apply(patch, &file)?;
    }
    if let Some(object) = values.get("flags") {
        let file = row_source("flags");
        let patch: FlagsPatch = from_json_object(&file, object)?;
        // ⚠ READ BEFORE THE APPLY: the patch is `Option<bool>` and the resolved field is a `bool`,
        // so `Some(false)` and `None` become the same value one line down.
        if patch.reconcile == Some(false) {
            settings.warnings.push(crate::flags::reconcile_refusal_ignored(&format!(
                "`reconcile = false` in the {}",
                file.display()
            )));
        }
        settings.flags.apply(patch, &file)?;
    }
    Ok(())
}

/// **One section's assembled object, deserialized as its patch type.**
///
/// The object is a `serde_json::Value` every leaf of which was parsed by [`parse_row_value`], so
/// this is the TYPE check and nothing else — serde's own `deny_unknown_fields` and visitors.
///
/// No redaction wrapper: unlike `toml::de::Error`, a `serde_json::Error` carries no input and its
/// `Display` renders no source snippet.
///
/// ⚠ **The KEY is found in TWO ways.** Serde's own *unknown field* message writes the name INSIDE
/// itself, so [`key_from_parse_message`]'s markers still catch the typo'd-key path — but a TYPE
/// MISMATCH renders as a bare `invalid type: string "30000", expected u64` with no name in it at
/// all. So when serde names no key, [`offending_leaf`] finds it by NARROWING.
fn from_json_object<T: for<'de> Deserialize<'de>>(
    file: &Path,
    object: &serde_json::Value,
) -> Result<T, ConfigError> {
    T::deserialize(object).map_err(|e: serde_json::Error| {
        let message = e.to_string();
        ConfigError::Parse {
            file: file.to_path_buf(),
            key: key_from_parse_message(&message).or_else(|| offending_leaf::<T>(object)),
            message,
        }
    })
}

/// **Which LEAF of an assembled section the type check refused**, for the messages that name none.
///
/// The narrowing is a re-run of the REAL check rather than a second reading of the message: each
/// leaf of the object is re-deserialized ALONE, under its own parent chain, through the same
/// `T::deserialize` that just failed — so the answer is the type model's, not a heuristic over
/// prose. The first leaf that refuses on its own names the row.
///
/// It is exact rather than a guess because of a property of the four patch types: **every field of
/// every one of them is an `Option`**, which is what `deny_unknown_fields` needs in order to mean
/// "unmentioned = inherit". A single-leaf object can therefore never fail for a MISSING field, so a
/// leaf that refuses alone refuses on its own merits.
///
/// Returns `None` when every leaf passes alone — the whole object then failed for something no one
/// row owns, and `key: None` is the honest answer.
fn offending_leaf<T: for<'de> Deserialize<'de>>(object: &serde_json::Value) -> Option<String> {
    json_leaves(object)
        .into_iter()
        .find(|(path, leaf)| T::deserialize(&nest(path, leaf)).is_err())
        .map(|(path, _)| path.join("."))
}

/// One LEAF of an assembled section: the path segments that reach it, and the value sitting there.
type Leaf<'a> = (Vec<String>, &'a serde_json::Value);

/// Every LEAF of an assembled section object, as its path segments and the value sitting there.
///
/// An EMPTY object is a leaf of its own — without it a table that is present and empty contributes
/// no path at all and the narrowing above would skip it silently.
fn json_leaves(object: &serde_json::Value) -> Vec<Leaf<'_>> {
    fn walk<'a>(prefix: &mut Vec<String>, value: &'a serde_json::Value, out: &mut Vec<Leaf<'a>>) {
        match value {
            serde_json::Value::Object(map) if !map.is_empty() => {
                for (key, child) in map {
                    prefix.push(key.clone());
                    walk(prefix, child, out);
                    prefix.pop();
                }
            }
            _ => out.push((prefix.clone(), value)),
        }
    }
    let mut out = Vec::new();
    walk(&mut Vec::new(), object, &mut out);
    out
}

/// [`json_leaves`]' inverse for ONE leaf — `["rate","max_utilization"]` and `0.4` back into
/// `{"rate":{"max_utilization":0.4}}`, the smallest object that still meets the real type check at
/// the place the row actually sits.
fn nest(path: &[String], leaf: &serde_json::Value) -> serde_json::Value {
    let mut built = leaf.clone();
    for segment in path.iter().rev() {
        built = serde_json::Value::Object(
            std::iter::once((segment.clone(), built)).collect::<serde_json::Map<_, _>>(),
        );
    }
    built
}

#[path = "mirror_tests.rs"]
#[cfg(test)]
mod mirror_tests;
