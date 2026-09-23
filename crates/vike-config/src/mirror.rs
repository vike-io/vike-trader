//! **The four settings files as ROWS, and back again** —
//! `docs/decisions/0057-the-seven-settings-files-answered-one-at-a-time.md`'s Phase 1.
//!
//! ⚠ **This module used to say "Nothing in this module changes an effective value on any box", and
//! since phase 1b that is FALSE** — [`apply_adopted_rows`] is what an adopted box resolves FROM.
//! The claim survives exactly where it was proven and nowhere else, which is the UNADOPTED box:
//! `crates/vike-config/tests/mirror.rs`'s
//! `a_store_with_no_adoption_row_changes_no_effective_value` resolves every settings directory it
//! can build twice (files alone, then files over the rows derived from them) and demands the two
//! [`Settings`] be equal, `warnings` included. The test was RENAMED to say so; the sentence above
//! it was not swept with it, and a stale invariant at the top of a module is what a reader checks
//! instead of the code.
//!
//! So the two dispositions, stated together because the difference is the whole of phase 1b:
//!
//! * **No adoption row — 0057 Phase 1, MIRRORED.** The store is written and the FILES still win.
//!   An unreadable row DEGRADES, because a file answers underneath it.
//! * **Adopted — the rows are the ONLY layer.** There is no file underneath, so an unreadable row
//!   is not free; it is marked on [`Settings::seal_refusal`] and the verbs that ACT on a ceiling
//!   refuse. It is not returned as `Err` from here, because that bricks the repair verbs — see
//!   [`apply_adopted_rows`].
//!
//! Two halves, and they are each other's inverse:
//!
//! * [`rows_from_files`] — read the four TOMLs, validate them exactly as a boot would, and render
//!   every scalar leaf as a `(section, dotted key, JSON scalar)` row. `policy.toml`'s `[venues]`
//!   and `[accounts]` maps are diverted into the ARMING rows instead (see below).
//! * [`section_values`] / [`apply_rows`] — PARSE each stored value on its own, assemble the
//!   section's JSON object out of the results, and deserialize **the existing patch type** from
//!   that object, so the read path gets its unknown-key refusal BY NAME from the same
//!   `deny_unknown_fields` a file gets it from, every bound stays imported from `vike-model`, and
//!   every tombstone keeps its own message.
//!
//! # ⚠ The value column holds a JSON SCALAR, and what that replaced
//!
//! It held a TOML rendering until 2026-09-18, and the read path then reassembled a synthetic TOML
//! *document* per section and re-parsed it — so one scalar an operator wrote as TOML was rendered
//! as TOML, stored as TOML text, spliced into TOML text again and parsed a second time. The owner
//! refused that chain. A stored value is now parsed by `serde_json` — **the parser decides the
//! type, not this module** — and the patch type is deserialized from the assembled object. No
//! document is rendered and no text is re-parsed.
//!
//! The property the document round trip existed to protect survives, and it survives because of
//! WHERE the parse happens rather than by assertion: **each row's value goes through
//! `serde_json::from_str` ON ITS OWN, before it is placed in the object**, so a malformed value
//! fails at its own row, naming that row's key. What a hand-assembled `toml::Table` would have
//! smuggled past the parser — a value that was never parsed at all — is unreachable here, because
//! there is no path into the object that does not go through a parse. This module's
//! `a_stale_row_degrades_by_name_rather_than_stopping_the_boot` and its illegal-row twin are the demonstration, and
//! `crates/vike-config/tests/mirror.rs`'s `a_stale_format_row_degrades_by_name_never_guessed`
//! drives it through the loader every root boots with.
//!
//! Two shapes JSON cannot carry and TOML can are refused rather than rendered — see [`flatten`] —
//! and the one shape JSON can carry and TOML cannot (`null`) is refused on the way IN, by
//! [`section_values`], because a `null` in an `Option<T>` field deserializes as *"this key is
//! unset"*: the exact silent read this module exists to prevent.
//!
//! # ⚠ Why the round trip has to go through the patch types
//!
//! 0057 states the cost of not doing it, and it is the difference between preserving the property
//! and claiming to: *"On the READ path a typo'd key written by any route other than the validated
//! writer — a hand `INSERT`, a botched migration, a restored backup — is a row that silently reads
//! as nothing, because there is no whole-document parse to refuse it."* A `setting` row is a typo'd
//! ROW KEY, not a typo'd column, so the database refuses it in neither direction and no `CHECK` can
//! be written that would. Materialising the section back into a value and deserializing it as
//! [`PolicyPatch`] / [`ConfigPatch`] / [`PreferencesPatch`] / [`FlagsPatch`] is what gives it back.
//!
//! # ⚠ The arming rows, and the one thing the mirror deliberately does NOT write
//!
//! `policy.venues` is 0057's *"the one place the tree's unknown-key refusal structurally cannot
//! reach"* — `deny_unknown_fields` governs a struct's FIELD names and serde treats a map's keys as
//! data — so it lands as its own table, a row per roster venue, always NAMED even when the value
//! equals the fallback. `[accounts]` rides the same table with a `label`, because it is the same
//! kind of object: an arming ceiling that can only ever refuse.
//!
//! **But only when the file declared a `[venues]` table at all.** A policy file with no such table
//! mirrors to ZERO arming rows, and that is not a shortcut — it is the no-effective-change property
//! doing real work. [`VenuePolicy::is_declared`](crate::venue_mode::VenuePolicy::is_declared) is a
//! fact about whether an operator ever stated an arming decision, `vike_mount::venue_arming_migration`
//! fires on it once at startup for a box with credentials and no `[venues]` table, and a roster of
//! all-`paper` rows would set it — silencing that warning for a deployment that has still not stated
//! one. The record asks for a roster-complete table; it asks harder for the mirror to change
//! nothing. So the table is roster-complete WHENEVER IT EXISTS, and it exists exactly when the file
//! said something.
//!
//! # This crate still never opens the store
//!
//! `crates/vike-config/Cargo.toml` declares its edge to `vike-secrets` with the words *"this crate
//! never opens the store, and must not"*, and 0057 adds the reason that matters under one database:
//! a handle that reaches the settings rows also reaches the `credential` table, from the crate whose
//! boot disclosure goes into a file shipped with bug reports. So [`StoredSettings`] arrives here as
//! DATA — a PARAMETER, exactly like the environment map — and the BINARY does the opening.
//! `crates/vike-secrets/src/settings.rs` is the other side of that split and carries the table
//! boundary.

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};

use serde::Deserialize;
use vike_secrets::{Adoption, ArmingRow, SettingRow, StoredSettings};

use crate::config::ConfigPatch;
use crate::error::{ConfigError, key_from_parse_message};
use crate::flags::FlagsPatch;
use crate::load::{
    CONFIG_FILE, FLAGS_FILE, POLICY_FILE, PREFERENCES_FILE, Settings, load, parse_toml_str,
};
use crate::policy::PolicyPatch;
use crate::preferences::PreferencesPatch;

/// `policy.toml`'s per-VENUE arming table. Diverted out of the flat rows into `venue_arming`.
///
/// The literal is `PolicyPatch::venues`' serde field name. It is spelled here rather than derived
/// because serde exposes no field-name constant, and the pair is held by
/// `crates/vike-config/tests/mirror.rs`'s
/// `the_diverted_keys_are_exactly_the_map_shaped_keys_of_the_policy_type`, which derives the set
/// from a real `Policy` serialization rather than trusting either spelling.
const VENUES_KEY: &str = "venues";
/// `policy.toml`'s per-ACCOUNT arming table — same divert, same gate. See [`VENUES_KEY`].
const ACCOUNTS_KEY: &str = "accounts";
/// `policy.toml`'s per-ACCOUNT EXPOSURE table — diverted for the same reason as the two above:
/// its key space is `{venue: {LABEL: figure}}`, which flattened into dotted `setting` rows would
/// carry no roster check at all. Its rows ride `venue_arming` beside the modes.
const ACCOUNT_EXPOSURE_KEY: &str = "account_exposure";

/// The four settings SECTIONS, paired with the file each one is read from and written back to.
///
/// One list rather than four `match` arms, so a section cannot be mirrored in one direction and
/// forgotten in the other. `vike_secrets::SETTINGS_SECTIONS` is the same vocabulary on the store's
/// side (and the `setting.section` `CHECK`'s); `crates/vike-config/tests/mirror.rs`'s
/// `the_sections_this_crate_mirrors_are_the_sections_the_store_accepts` holds the two equal.
pub const SECTION_FILES: [(&str, &str); 4] = [
    ("policy", POLICY_FILE),
    ("config", CONFIG_FILE),
    ("preferences", PREFERENCES_FILE),
    ("flags", FLAGS_FILE),
];

/// The settings file a section's rows materialise back into — the name that appears in a refusal,
/// so an operator reading *"unknown field `max_levrage`"* is told which schema refused it.
#[must_use]
pub fn file_for_section(section: &str) -> Option<&'static str> {
    SECTION_FILES.iter().find(|(s, _)| *s == section).map(|(_, f)| *f)
}

/// What a refusal raised against a materialised ROW names instead of a path on disk.
///
/// A `ConfigError` names a FILE, and these keys came from a table. Naming the file they WOULD have
/// been in would be a lie an operator acts on (they would open it and find nothing); naming nothing
/// would lose the schema. So the pseudo-path says both: the store, and the section whose schema
/// refused.
fn row_source(section: &str) -> PathBuf {
    PathBuf::from(format!("settings database (section `{section}`)"))
}

// ---------------------------------------------------------------------------------------------
// Files -> rows
// ---------------------------------------------------------------------------------------------

/// **Read the four settings files and render them as store rows.**
///
/// The load runs FIRST, with an empty environment, so a directory that a boot would refuse is
/// refused here with the same error naming the same file and key — a mirror of an invalid file is
/// a store that would refuse every later read, written by a command that reported success.
///
/// The empty environment is load-bearing and is not a convenience: the rows must be a function of
/// the FILES alone. An env layer folded in here would write an exported variable's value into the
/// store as though an operator had filed it, which is the shape of `policy.toml`'s own sealed-layer
/// argument wearing a different carrier.
pub fn rows_from_files(settings_dir: &Path) -> Result<StoredSettings, ConfigError> {
    let settings = load(Some(settings_dir), &HashMap::new())?;
    rows_from_loaded(settings_dir, &settings)
}

/// [`rows_from_files`] over a [`Settings`] the caller has already loaded from the SAME directory
/// with no environment layer. Split out so a caller that must load once does not load twice.
fn rows_from_loaded(
    settings_dir: &Path,
    settings: &Settings,
) -> Result<StoredSettings, ConfigError> {
    let mut rows = Vec::new();
    for (section, file) in SECTION_FILES {
        let path = settings_dir.join(file);
        let Some(table) = read_raw(&path)? else { continue };
        for (key, value) in &table {
            // The THREE POLICY maps are arming ceilings, not settings keys — see this module's doc.
            if section == "policy"
                && (key == VENUES_KEY || key == ACCOUNTS_KEY || key == ACCOUNT_EXPOSURE_KEY)
            {
                continue;
            }
            flatten(&path, section, key, value, &mut rows)?;
        }
    }
    rows.sort();

    let mut arming = Vec::new();
    // ⚠ `is_declared`, never "the map is non-empty": the compiled-in map already carries every
    // roster venue at `paper`. See this module's doc for what writing those rows anyway would cost.
    // ⚠ **The per-ACCOUNT exposure figures ride these same rows**, because they are arming ceilings
    // by this store's own test — they compose by `min` and can never raise anything
    // (`vike_secrets::settings`'s module doc states the test; `PolicyPatch::account_exposure`
    // applies it). The venue-level row carries the UNLABELLED account's figure, which is why
    // `[account_exposure]` admits the `DEFAULT` spelling that `[accounts]` refuses: on this table
    // `label IS NULL` already means both "the venue's ceiling" and "the unlabelled account's".
    let exposure = settings.policy.venues.account_exposure_by_venue();
    let figure = |venue: &str, label: &str| -> Option<f64> {
        exposure.get(venue).and_then(|by_label| by_label.get(label)).copied()
    };
    // ⚠ `is_declared`, never "the map is non-empty": the compiled-in map already carries every
    // roster venue at `paper`. See this module's doc for what writing those rows anyway would cost.
    if settings.policy.venues.is_declared() {
        for (venue, mode) in settings.policy.venues.iter() {
            arming.push(ArmingRow {
                venue: venue.to_string(),
                label: None,
                mode: mode.as_str().to_string(),
                max_exposure: figure(venue, vike_model::account_keys::RESERVED_DEFAULT_LABEL),
            });
        }
    }
    for (venue, labels) in settings.policy.venues.accounts_by_venue() {
        for (label, mode) in labels {
            let cap = figure(venue, &label);
            arming.push(ArmingRow {
                venue: venue.to_string(),
                label: Some(label),
                mode: mode.as_str().to_string(),
                max_exposure: cap,
            });
        }
    }
    // Keyed, for the reason `vike_secrets::ArmingRow` no longer derives a total order — see its own
    // note. Same key the reader's `ORDER BY` uses, so a mirrored store and a read-back one sort
    // identically.
    arming.sort_by(|a, b| {
        (a.venue.as_str(), a.label.as_deref()).cmp(&(b.venue.as_str(), b.label.as_deref()))
    });

    // ⚠ **EMPTY, and that is not an omission — it is the reason `venue_setting` is a table.** This
    // function re-derives rows FROM THE SETTINGS FILES, and venue settings have no file to be
    // derived from: `Config` carries `#[serde(deny_unknown_fields)]` and has no `venue` field, so
    // `config.toml` cannot spell one. `vike_secrets::write_settings_in` therefore does not clear
    // that table either — its own ⚠ says what a `DELETE FROM venue_setting` beside the other two
    // would cost — so mirroring leaves a box's JForex server, IBKR gateway and polymarket egress
    // exactly where the move put them.
    Ok(StoredSettings { settings: rows, arming, venue: Vec::new() })
}

/// Read one settings file as a raw table. `Ok(None)` when it is absent — the same absent-layer rule
/// [`crate::load`] applies, reached the same way (on the READ result, never a prior `exists()`).
fn read_raw(file: &Path) -> Result<Option<toml::Table>, ConfigError> {
    let text = match std::fs::read_to_string(file) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(source) => return Err(ConfigError::Read { file: file.to_path_buf(), source }),
    };
    parse_toml_str::<toml::Table>(file, &text).map(Some)
}

/// One key of a parsed settings file into zero or more rows: a table recurses with a dotted prefix,
/// a scalar renders itself as JSON, and anything JSON cannot carry is refused by name.
fn flatten(
    file: &Path,
    section: &str,
    key: &str,
    value: &toml::Value,
    out: &mut Vec<SettingRow>,
) -> Result<(), ConfigError> {
    match value {
        toml::Value::Table(inner) => {
            for (child, v) in inner {
                flatten(file, section, &format!("{key}.{child}"), v, out)?;
            }
            Ok(())
        }
        // ⚠ No settings key in this crate's model is an array today, and the refusal is deliberate
        // rather than a `todo!`: an array flattened to one row would round-trip only by accident,
        // and a mirror that silently mangled a value would be worse than one that refuses to run.
        // The day a key needs one, this arm is where the shape is decided.
        toml::Value::Array(_) => Err(ConfigError::Parse {
            file: file.to_path_buf(),
            key: Some(key.to_string()),
            message: format!(
                "`{key}` is an ARRAY, and the settings store holds one scalar per row. A key with \
                 an array value needs a row shape decided for it before it can be mirrored; \
                 nothing has been written."
            ),
        }),
        scalar => match json_scalar(scalar) {
            Some(rendered) => {
                out.push(SettingRow {
                    section: section.to_string(),
                    key: key.to_string(),
                    value: rendered,
                });
                Ok(())
            }
            // ⚠ The two shapes TOML carries and JSON does not. Neither is reachable from a settings
            // file this tree accepts — `rows_from_files` runs `load` FIRST, every `f64` field is
            // bounded or `is_finite`-checked, and a datetime type-errors against every field the
            // four patch types have — but that is a property of TODAY'S FIELD SET, not of this
            // module, which is exactly why the arm refuses instead of rendering. An `f64` field
            // added tomorrow without a finiteness check would otherwise mirror a ceiling of `inf`
            // as the JSON `null` `serde_json` renders it as, and `null` reads back as UNSET: a
            // ceiling that VANISHED rather than one that was refused.
            None => Err(ConfigError::Parse {
                file: file.to_path_buf(),
                key: Some(key.to_string()),
                message: format!(
                    "`{key}` is {}, and the settings store holds one JSON SCALAR per row. JSON has \
                     no form for it, and rendering it as one that reads back as a different type \
                     would be worse than a refusal; nothing has been written.",
                    unrenderable_shape(scalar)
                ),
            }),
        },
    }
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
fn json_scalar(value: &toml::Value) -> Option<String> {
    match value {
        toml::Value::String(s) => Some(serde_json::Value::String(s.clone()).to_string()),
        toml::Value::Integer(i) => Some(i.to_string()),
        toml::Value::Boolean(b) => Some(b.to_string()),
        toml::Value::Float(f) => serde_json::Number::from_f64(*f).map(|n| n.to_string()),
        toml::Value::Datetime(_) | toml::Value::Array(_) | toml::Value::Table(_) => None,
    }
}

/// How a value [`json_scalar`] refused is NAMED in the refusal — an operator is told which shape of
/// their file the store cannot hold, not merely that something failed.
fn unrenderable_shape(value: &toml::Value) -> &'static str {
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
/// ⚠ **The two answers are DISPOSITIONS, not severities.**
///
/// `crates/vike-boot/src/lib.rs`'s settings arm states the rule this splits on: *could this layer be
/// read at all* DEGRADES, *does this layer say something illegal* REFUSES. A settings store is a
/// MIRROR for as long as 0057 Phase 1 lasts — the files win — so a layer nobody can read costs the
/// disclosure it would have provided and nothing else, while a layer that says something no release
/// ever wrote is evidence of a hand `INSERT`, a botched migration or a restored backup and must stop
/// a daemon rather than be resolved around.
///
/// ⚠ Collapsing the two is not a style question. An earlier draft refused BOTH, and a row in the
/// previous release's own encoding then took every binary down — including the one command whose
/// name the refusal printed. [`parse_row_value`] carries that measurement.
#[derive(Debug)]
pub enum RowRefusal {
    /// The row is not JSON at all. Almost always a store written before the value column became
    /// JSON. DEGRADE: drop the whole store layer, warn naming the repair, let the files answer.
    Unreadable(String),
    /// The row parsed and says something no mirror writes. REFUSE.
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
/// * a JSON `null`, which is the one thing JSON can express and TOML cannot, and which
///   `deny_unknown_fields` structurally cannot catch: serde sees a KNOWN field name and an
///   `Option<T>` field reads `null` as `None`, so a key an operator filed would silently resolve to
///   its default;
/// * a dotted key that collides with another row — `a` and `a.b` together, which the TOML document
///   this replaced was refused for by the parser and which `UNIQUE (section, key)` cannot see.
pub fn section_values(
    store: &StoredSettings,
) -> Result<BTreeMap<&'static str, serde_json::Value>, RowRefusal> {
    let mut by_section: BTreeMap<&'static str, serde_json::Map<String, serde_json::Value>> =
        BTreeMap::new();
    for row in &store.settings {
        let found =
            SECTION_FILES.iter().find(|(s, _)| *s == row.section).map(|(section, _)| *section);
        let Some(section) = found else {
            // A section the four-word vocabulary does not carry. `setting.section`'s own `CHECK`
            // refuses one, so it is unreachable through every write this tree has — a hand
            // `INSERT` against a schema that has been altered is the only way to produce it, and
            // skipping rather than panicking keeps that from taking a daemon down.
            //
            // ⚠ **DECLARED RESIDUAL: such a row is read as nothing and NOTHING NAMES IT.** That is
            // the one place this module does not hold the read-path property it was written for —
            // `config check`'s store finding counts rows and does not classify their sections, and
            // `config show`'s ORIGIN column can only report a key it resolved.
            //
            // ⚠ **The justification for leaving it open CHANGED on 2026-09-18 and the old one must
            // not be quoted.** It read *"a REFUSAL this mirror period has no appetite for (the
            // files still answer)"* — and the mirror period is exactly what 0057's crossing ends.
            // On an ADOPTED box the files do NOT answer, so an unknown-section row is the only copy
            // of whatever it holds and reading it as nothing is precisely the failure this module
            // exists to prevent.
            //
            // What survives of the argument is narrower and is what keeps this a `continue` rather
            // than a refusal: the row is unreachable through every write this tree has, so refusing
            // it would only ever fire on a store somebody altered the SCHEMA of by hand — and
            // refusing there strands a daemon while `config mirror`, the repair, would have fixed
            // it. The honest disposition is to NAME it, which is `config check`'s job and not this
            // reader's, and which is what the row-set finding there does.

            continue;
        };
        let file = row_source(section);
        let value = parse_row_value(&file, &row.key, &row.value)?;
        insert_at(&file, by_section.entry(section).or_default(), &row.key, value)?;
    }

    // The arming rows are `policy.toml`'s two tables. They carry a CLOSED vocabulary — a roster
    // venue id, an `AccountLabel`, and one of three mode words — so they are placed as VALUES with
    // no rendering step and no key quoting: `mode` is `venue_arming`'s own `CHECK` list, and the
    // refusal for a word outside it is `Policy::apply`'s, reached below through the patch type.
    //
    // ⚠ They go in at the TOP level by name rather than through [`insert_at`]'s dotted splitter,
    // which is the one place a label with a `.` in it would otherwise be split into a path. The
    // alphabet refuses one today; the placement is what keeps that from being load-bearing.
    let policy_file = row_source("policy");
    let venue_rows: Vec<&ArmingRow> = store.arming.iter().filter(|r| r.label.is_none()).collect();
    if !venue_rows.is_empty() {
        let mut venues = serde_json::Map::new();
        for row in venue_rows {
            venues.insert(row.venue.clone(), serde_json::Value::String(row.mode.clone()));
        }
        let section = by_section.entry("policy").or_default();
        insert_top_level(&policy_file, section, VENUES_KEY, serde_json::Value::Object(venues))?;
    }
    let mut accounts: BTreeMap<&str, serde_json::Map<String, serde_json::Value>> = BTreeMap::new();
    for row in store.arming.iter().filter(|r| r.label.is_some()) {
        let label = row.label.as_deref().unwrap_or_default();
        accounts
            .entry(row.venue.as_str())
            .or_default()
            .insert(label.to_string(), serde_json::Value::String(row.mode.clone()));
    }
    if !accounts.is_empty() {
        let by_venue: serde_json::Map<String, serde_json::Value> = accounts
            .into_iter()
            .map(|(venue, labels)| (venue.to_string(), serde_json::Value::Object(labels)))
            .collect();
        let section = by_section.entry("policy").or_default();
        insert_top_level(&policy_file, section, ACCOUNTS_KEY, serde_json::Value::Object(by_venue))?;
    }
    // ⚠ **The exposure figures come back out of the SAME rows**, keyed the way the forward
    // direction put them in: a `label IS NULL` row's figure is the UNLABELLED account's and comes
    // back under the reserved `DEFAULT` spelling, a labelled row's under its own label. Without
    // this block the round trip would be lossy in the one direction that matters — the store would
    // hold the figure and `config adopt` would resolve a policy that does not, so the ceiling would
    // vanish at exactly the moment the store becomes the authority.
    let mut caps: BTreeMap<&str, serde_json::Map<String, serde_json::Value>> = BTreeMap::new();
    for row in store.arming.iter().filter(|r| r.max_exposure.is_some()) {
        let Some(cap) = row.max_exposure.and_then(serde_json::Number::from_f64) else {
            continue;
        };
        let label = row
            .label
            .as_deref()
            .unwrap_or(vike_model::account_keys::RESERVED_DEFAULT_LABEL)
            .to_string();
        caps.entry(row.venue.as_str()).or_default().insert(label, serde_json::Value::Number(cap));
    }
    if !caps.is_empty() {
        let by_venue: serde_json::Map<String, serde_json::Value> = caps
            .into_iter()
            .map(|(venue, labels)| (venue.to_string(), serde_json::Value::Object(labels)))
            .collect();
        let section = by_section.entry("policy").or_default();
        insert_top_level(
            &policy_file,
            section,
            ACCOUNT_EXPOSURE_KEY,
            serde_json::Value::Object(by_venue),
        )?;
    }

    Ok(by_section.into_iter().map(|(s, o)| (s, serde_json::Value::Object(o))).collect())
}

/// **One stored value, parsed as one complete JSON document.**
///
/// `serde_json::from_str` rejects TRAILING INPUT, and that is load-bearing rather than incidental:
/// it is what keeps a row whose value is `3.0, "max_notional_per_order": 9e9` from setting a
/// SECOND key. (The TOML text-splice this replaced was equally injectable and equally reliant on
/// its parser to refuse the splice; the property is carried, not introduced.)
///
/// The message never echoes the value. `serde_json::Error`'s `Display` has no input to echo — it
/// renders `expected value at line 1 column 1`, with no annotated snippet — which is the property
/// `crate::error::redacted_parse_message` has to REMOVE from `toml::de::Error` by hand, and it is
/// why this path needs no redaction of its own. A settings directory sits beside `secrets.env`, so
/// a credential written into the wrong file is a plausible first-time mistake and the refusal for
/// it must not be the thing that publishes it.
fn parse_row_value(file: &Path, key: &str, raw: &str) -> Result<serde_json::Value, RowRefusal> {
    let value = serde_json::from_str::<serde_json::Value>(raw).map_err(|e| {
        // ⚠ **UNREADABLE, not ILLEGAL — and the distinction is what keeps this from bricking a box.**
        // `crates/vike-boot/src/lib.rs`'s settings arm already states the rule: *could this layer be
        // read at all* DEGRADES, *does this layer say something illegal* REFUSES. A value in the
        // PREVIOUS RELEASE's encoding is the first question, and an earlier draft of this function
        // answered it with the second.
        //
        // What that cost, MEASURED 2026-09-18 with real binaries on both commits: a `config.toml`
        // holding a Windows path mirrors through `toml_writer`, whose `TomlStringBuilder::as_default`
        // falls to `as_literal` whenever a backslash sets the escape metric — so the row reads
        // `'C:\vike\state'`, valid TOML and not JSON. On upgrade every vike binary refused to start,
        // `vike-cli config mirror` INCLUDED — the one command the refusal told the operator to run —
        // and `sqlite3` is installed on neither box. The dev box is Windows.
        //
        // ⚠ **This function is shared by BOTH dispositions, so it decides neither.** It returns a
        // `RowRefusal` and the CALLER says what that costs — which is why the justification below
        // is scoped to the caller it is true for, having previously been written here flat ("this
        // is 0057 Phase 1, the FILES still win") where the adopted caller reads it and it is wrong.
        //
        // For [`apply_rows`] (no adoption row) degrading is free and is not a compromise: the FILES
        // still win, and `crates/vike-config/tests/mirror.rs`'s
        // `a_store_with_no_adoption_row_changes_no_effective_value` is the proof that a store layer
        // contributes nothing a file does not already decide. A layer nobody can read costs exactly
        // the disclosure it would have provided.
        //
        // For [`apply_adopted_rows`] it is NOT free — there is no file underneath — so that caller
        // marks [`Settings::seal_refusal`] instead of swallowing it, and the verbs that act on a
        // ceiling refuse while every repair verb keeps running.
        RowRefusal::Unreadable(format!("{}: `{key}` — {e}. {STALE_FORMAT_HINT}", file.display()))
    })?;
    // ⚠ A JSON integer `serde_json` DEMOTED to a float, because it fits neither `i64` nor `u64`.
    // The synthetic TOML document this replaced refused such a row outright (`toml::Value::Integer`
    // is `i64`), and without this arm the demotion is silent and lands on a CEILING: measured, a
    // `max_notional_per_order` row of `18446744073709551616` resolves to `Some(1.84e19)`, passes
    // `check_positive` (positive and finite), and leaves the per-order cap effectively absent while
    // `config show` reports it resolved. ILLEGAL rather than unreadable — the row is well-formed
    // JSON, `json_scalar` renders from an `i64` and so can never write one, and no release ever did.
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
                 believe they filed a cap and have none. The mirror renders integers from an \
                 `i64` and cannot produce this: a hand `INSERT`, a botched migration or a restored \
                 backup did."
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
                 `deny_unknown_fields` cannot catch it because the FIELD NAME is known. TOML had no \
                 way to say it, so no mirror wrote one: a hand `INSERT`, a botched migration or a \
                 restored backup did."
            ),
        }));
    }
    Ok(value)
}

/// What a refusal adds when a stored value does not parse — the one sentence that turns *"this row
/// is broken"* into *"this row is in the format the column held before"*, and names the act that
/// fixes it.
///
/// It is a HINT rather than a detection, deliberately: this module cannot tell a pre-JSON rendering
/// from a typo, and claiming to would be a guess. What it can say is that the rows are a
/// REGENERABLE mirror of files that still win, so re-deriving them is always safe and always
/// correct. MEASURED: of the renderings the old column could hold, every one is either invalid JSON
/// (refused here, loudly) or parses to the SAME value — `crates/vike-config/tests/mirror.rs`'s
/// `no_pre_json_rendering_reads_back_as_a_different_value` is that proof, and it is why this is a
/// refusal or a no-op and never a silently different ceiling.
const STALE_FORMAT_HINT: &str = "A `setting` row holds ONE JSON SCALAR (`100`, `\"warn\"`, `true`). \
                                 A store mirrored before the value column became JSON holds TOML \
                                 renderings, and the ones JSON has no form for land here: re-run \
                                 `vike-cli config mirror` to re-derive every row from the files, \
                                 which still win.";

/// Place one parsed value into its section's object at a DOTTED key — `rate.max_utilization` into
/// `{"rate":{"max_utilization":…}}` — refusing a collision rather than resolving one.
///
/// ⚠ The collision refusal is the half that is not obvious, and it is a property the TOML document
/// carried for free: `a = 1` beside `a.b = 2` is a document the parser refuses, while a hand-built
/// object would silently keep whichever row came last. `UNIQUE (section, key)` on the table cannot
/// see it either — the two rows have different keys. So it is checked here, by name.
///
/// ⚠ A row key that literally CONTAINS a dot is indistinguishable from a nested path. That is
/// carried from the rendering this replaced (whose `key_path` split on `.` per segment in exactly
/// the same way), not introduced by it, and no key in the four patch types is spelled with one.
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

/// [`insert_at`] for a key that must be taken WHOLE — the two arming tables, whose names are
/// compile-time constants and whose contents were never dotted paths.
fn insert_top_level(
    file: &Path,
    object: &mut serde_json::Map<String, serde_json::Value>,
    key: &str,
    value: serde_json::Value,
) -> Result<(), ConfigError> {
    if object.contains_key(key) {
        return Err(collision(file, key));
    }
    object.insert(key.to_string(), value);
    Ok(())
}

/// The refusal both inserters raise: two rows of one section claim the same place in its object.
fn collision(file: &Path, key: &str) -> ConfigError {
    ConfigError::Parse {
        file: file.to_path_buf(),
        key: Some(key.to_string()),
        message: format!(
            "`{key}` collides with another row of this section — a key and a path THROUGH that key \
             cannot both be set, and the settings file this section mirrors would be refused by its \
             own parser for the same pair. Nothing has been read."
        ),
    }
}

/// **Apply a store's rows to `settings` BELOW the files — the UNADOPTED path.**
///
/// This is 0057 Phase 1's disposition and it is unchanged: the files win, so a layer nobody can
/// read costs the disclosure it would have provided and nothing else. A store in a PREVIOUS
/// ENCODING therefore DEGRADES — `Ok`, having applied nothing, with a warning on
/// `settings.warnings`.
///
/// ⚠ **On an ADOPTED store this is the wrong function and using it would be the unmount.** See
/// [`apply_adopted_rows`], which is the same apply with the opposite disposition and an integrity
/// check in front of it.
pub fn apply_rows(settings: &mut Settings, store: &StoredSettings) -> Result<(), ConfigError> {
    let values = match section_values(store) {
        Ok(v) => v,
        Err(RowRefusal::Unreadable(why)) => {
            settings.warnings.push(format!(
                "the settings database was NOT read: {why} Nothing this box resolves has changed — \
                 the files still win on a box that has not run `vike-cli config adopt` — but \
                 `vike-cli config show` will report no `db` origin until the store is re-derived."
            ));
            return Ok(());
        }
        Err(RowRefusal::Illegal(e)) => return Err(e),
    };
    apply_section_values(settings, &values)
}

/// **Apply a store's rows as THE ONLY settings layer — the ADOPTED path.**
///
/// Two things differ from [`apply_rows`], and both are the whole point of the seal:
///
/// 1. **[`RowRefusal::Unreadable`] INVERTS from a degrade into a refusal.** Carrying the degrade
///    across the crossing would BE the unmount this design exists to prevent: one unparseable row
///    would drop the whole layer, `Policy::default()` would answer, and that is `max_notional_per_order`
///    absent on a daemon whose control channel is armed AND `VenuePolicy::is_declared()` false, so
///    every venue caps to `paper` and `vike_mount::venue_arming_migration`'s banner re-fires on a
///    box that stated its arming months ago. Two quiet failures from one silent line.
///
///    The inversion is licensed by POSITIVE EVIDENCE rather than by preference: `vike-cli config
///    adopt` re-reads EVERY row through this same reader and refuses to write the seal if any row
///    is unreadable. So an adopted store holding an unreadable row means the rows changed after
///    adoption by some route other than the validated writer — exactly the hand-`INSERT` class
///    [`RowRefusal::Illegal`] already refuses for.
///
///    ⚠ It does NOT mean the three-hour-old JSON incident's lesson is repudiated. That lesson was
///    never *"always degrade"*; it was ***"a refusal must not brick the tool that repairs it"***,
///    and it is carried structurally instead: `vike-cli config mirror` re-derives the rows from the
///    files through [`rows_from_files`], which calls `crate::load` with NO store and therefore
///    cannot meet this refusal at all, and `vike-cli config adopt --undo` deletes one row without
///    going through a loader. Both repairs are clear of the refusal BY CONSTRUCTION.
///
/// 2. **The seal's own integrity is checked first** — see [`adoption_integrity`].
pub fn apply_adopted_rows(settings: &mut Settings, store: &StoredSettings, adoption: &Adoption) {
    // ⚠ **NOTHING here returns `Err`, and that is the whole repair.** This function used to end
    // every arm below in one, `load_with_source` propagated it with `?`, `vike_boot::boot` mapped
    // it to `Err(String)`, and `vike_cli::run` — which calls `resolve_policy` before `dispatch`,
    // for EVERY subcommand — printed it and returned `ExitCode::FAILURE`. Measured with real
    // binaries at `650907a37`: `config show`, `config mirror`, `config adopt --undo`,
    // `config check`, `secrets list`, `config compare` and `--help` all exited 1, and `sqlite3` is
    // installed on neither box, so the store could not be repaired out of band either. That is the
    // 2026-09-18 JSON incident reproduced in a different column, and `resolve_policy`'s own comment
    // had already written down why it must not be: *a refusal here would take down `config
    // mirror`, `config adopt --undo`, `config check` and every `secrets` verb — the exact commands
    // whose names the refusal prints.*
    //
    // So the refusal becomes a MARK, exactly as the unreadable-STORE arm above it does, and the
    // enforcement moves to the points that can act on it without being the repair:
    // [`Settings::seal_refusal`] names them.
    let warnings = match adoption_integrity(store, adoption) {
        Ok(warnings) => warnings,
        Err(e) => {
            settings.mark_seal_refusal(e.to_string());
            Vec::new()
        }
    };
    settings.warnings.extend(warnings);
    let values = match section_values(store) {
        Ok(v) => v,
        Err(RowRefusal::Unreadable(why)) => {
            settings.mark_seal_refusal(seal_refusal(format!(
                "{why} This box is ADOPTED, so these rows are the ONLY settings layer and there is \
                 no file underneath them to answer instead: every value below is a compiled-in \
                 default, which means NO CEILING and every venue capped `paper` with nothing \
                 recording that an arming was ever stated. `vike-cli config adopt` verified every \
                 row when it sealed this store, so a row it cannot read now arrived by some route \
                 other than the validated writer."
            )).to_string());
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

/// **The SEAL's own integrity — the ERASE and UNMOUNT detectors, checked at every adopted boot.**
///
/// Three questions, each one a shape of silence the crossing would otherwise create:
///
/// * **The counts.** `setting_rows`/`arming_rows` as sealed must equal the tables' actual counts.
///   Every sanctioned write updates them inside the transaction that changes the tables, so a
///   mismatch in EITHER direction means rows arrived or left by some other route. This is what
///   makes *armed and uncapped through row loss* unrepresentable — and it costs no behaviour change
///   on a box that never set a ceiling, because the counts are relative to what THIS box adopted
///   rather than to an absolute expectation.
/// * **`venues_declared`.** A box that had STATED an arming must still have arming rows. A
///   `policy.toml` with no `[venues]` table mirrors to ZERO venue rows deliberately, so once the
///   rows are the only layer *nobody ever stated one* and *the rows were erased* are the same empty
///   table, and only the seal can tell them apart.
/// * **Roster completeness.** `vike-cli config mirror` writes a venue row for EVERY
///   `vike_model::VENUES` id when the file declares, and none at all when it does not (gated by
///   `crates/vike-config/tests/mirror.rs`'s `a_stated_venues_table_mirrors_a_row_for_every_roster_venue`).
///   So *complete or empty* is an invariant a READ can check, and a partial table means somebody
///   deleted rows — which on this path silently caps the missing venues to `paper`.
///
/// ⚠ **COUNTS, deliberately, and not a per-row digest.** A digest would make the store
/// tamper-evident and would also refuse a boot over a hand-edited `preferences.log_file_level`, on
/// boxes where the repair would have to be the binary itself — the JSON incident with a different
/// value in the column. Counts are the largest check that cannot brick a box over a LEGAL value.
/// What that leaves silent is stated rather than hidden: **a value changed IN PLACE to another
/// legal value keeps the count and does not refuse**, and `config show` then attributes it to `db`
/// with full confidence, which is correct and is the problem. It is bounded — the pre-trade
/// ceilings that judge every order live in the run profile's `[risk]` table and are read from the
/// FILE (`vike_mount::require_live_risk_budget`), so an in-place row edit can move the
/// control-channel cap and the arming ceilings and cannot silently uncap the order path itself.
///
/// This runs ONLY on an adopted store. An unadopted box is byte-identical to before the seal
/// existed, which is why R2 can ship to every box in the world and change nothing on any of them.
pub fn adoption_integrity(
    store: &StoredSettings,
    adoption: &Adoption,
) -> Result<Vec<String>, ConfigError> {
    let settings_now = store.settings.len();
    let arming_now = store.arming.len();
    if settings_now != adoption.setting_rows || arming_now != adoption.arming_rows {
        return Err(seal_refusal(format!(
            "the settings rows have CHANGED since this box was adopted: it was sealed on {} \
             setting row(s) and {} arming row(s) and now carries {settings_now} and {arming_now}. \
             Every sanctioned write updates those counts in the same transaction that changes the \
             tables, so rows arrived or left by some other route — a hand `INSERT`/`DELETE`, a \
             restored backup, or a partially-applied migration. These rows are the ONLY settings \
             layer on this box, so the missing ones are not defaults an operator chose: they are a \
             ceiling or an arming decision that has silently stopped being applied.",
            adoption.setting_rows, adoption.arming_rows
        )));
    }

    let venue_rows = store.arming.iter().filter(|r| r.label.is_none()).count();
    if adoption.venues_declared && venue_rows == 0 {
        // Reachable, and the count check above does NOT subsume it: `arming_rows` counts every
        // arming row including the labelled account ones, so a store whose venue rows were
        // replaced one-for-one by account rows keeps the sealed count and arrives here.
        return Err(seal_refusal(
            "this box had STATED a per-venue arming when it was adopted and now carries no \
             venue-arming rows at all. An empty table resolves to every roster venue capped \
             `paper` with `is_declared()` false — which is also exactly what a box that never \
             stated one looks like, so without this refusal the two would be the same silence."
                .to_string(),
        ));
    }

    // ⚠ **A roster GAP is a WARNING, never a refusal, and the reason is that this comparison is
    // against the COMPILED roster while the seal is a fact about a PAST one.** As a refusal it was
    // reachable with no operator act at all: `rows_from_files` writes one row per
    // `vike_model::VENUES` id, so a box seals at exactly today's `VENUES.len()`, and the next
    // release that runs `just new-venue` makes the compiled roster one longer than the sealed
    // table. Measured at `650907a37`: a store sealed on 14 rows, one id added at `venues.rs`'s
    // marker, and every binary on the box refused to start — `config mirror` and
    // `config adopt --undo`, the two repairs this refusal's own text names, among them. Nothing
    // had been deleted; the roster had grown.
    //
    // The ERASE detector is the COUNT check above, which is seal-relative and therefore cannot
    // make that mistake. What this check adds over it is only the NAMES, and a venue with no row
    // resolves to `paper` — the safe direction — so naming them is worth a warning and cannot be
    // worth taking the box down.
    let mut warnings = Vec::new();
    if venue_rows > 0 && venue_rows != vike_model::VENUES.len() {
        let present: std::collections::BTreeSet<&str> =
            store.arming.iter().filter(|r| r.label.is_none()).map(|r| r.venue.as_str()).collect();
        let missing: Vec<&str> =
            vike_model::VENUES.iter().copied().filter(|v| !present.contains(v)).collect();
        if !missing.is_empty() {
            warnings.push(format!(
                "the venue-arming table carries {venue_rows} row(s) for a compiled roster of {}, \
                 so {missing:?} resolve to `paper` on this box. Either this binary is newer than \
                 the seal and those venues are simply new to the roster — the ordinary case after \
                 a release that adds one — or their rows were deleted. `vike-cli config mirror` \
                 writes a row for every roster venue and makes the table complete again either \
                 way.",
                vike_model::VENUES.len()
            ));
        }
    }

    Ok(warnings)
}

/// The shape every [`adoption_integrity`] refusal takes: a `ConfigError` naming the STORE rather
/// than a path on disk, exactly as an illegal row's does — see [`row_source`].
fn seal_refusal(message: String) -> ConfigError {
    ConfigError::Parse {
        file: row_source("policy"),
        key: None,
        // ⚠ **Every seal refusal names BOTH repairs, in one place**, because a refusal whose text an
        // operator cannot act on is the failure mode this whole family was written against: the
        // JSON incident's message named `vike-cli config mirror` and that command was down with
        // everything else. Both of these genuinely run in every refusing state, and structurally
        // rather than by care — `config mirror` resolves the files with no store at all, and
        // `config adopt --undo` deletes one row without going through a loader.
        //
        // ⚠ And neither may ever become *delete the database*. `vike_secrets::Backend` answers
        // `Files` for CREDENTIALS the moment that file is gone, so the folk repair takes every
        // venue on a migrated box silently to paper and destroys the only copy of its venue keys.
        // `crates/vike-config/tests/mirror.rs`'s `assert_no_catastrophic_repair` is the gate.
        message: format!(
            "{message} Repair with `vike-cli config mirror`, which re-derives every row from the \
             settings files; or step back to the files with `vike-cli config adopt --undo`. BOTH \
             still run in this state, and `vike-cli config compare` shows what the two sources \
             disagree about."
        ),
    }
}

/// The apply itself, shared by both dispositions above so the two paths cannot drift about what a
/// row MEANS — only about what a row that cannot be read COSTS.
///
/// Every section's assembled OBJECT is deserialized as its patch and handed to the same `apply` a
/// file layer uses, so an unknown key is refused BY NAME, a tombstoned key raises its own message,
/// and every bound is the one `vike-model` exports. The pseudo-file in the error names the STORE
/// and the section rather than a path on disk — see [`row_source`].
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
        // ⚠ READ BEFORE THE APPLY, for the reason `crate::load` gives at its own flags arm: the
        // patch is `Option<bool>` and the resolved field is a `bool`, so `Some(false)` and `None`
        // are the same value one line down.
        if patch.reconcile == Some(false) {
            // ⚠ `settings.authority` rather than a literal, and THIS is the call site that
            // proved the class. [`apply_section_values`] is reached from [`apply_rows`] (a
            // MIRRORED store below the files) and from [`apply_adopted_rows`] (a SEALED store
            // that is the only settings layer) alike — and in the second case the remedy this
            // warning used to print, *write `reconcile_off = true` into
            // `<project>/settings/flags.toml`*, named a file that box does not read for
            // resolution at all. A row saying `reconcile = false` is also far likelier to exist
            // on an adopted box than a stale `flags.toml` line is on an unadopted one, so the
            // operator most likely to see this line was the one the instruction could not help.
            // `crate::remedy` carries the class and the measurement.
            settings.warnings.push(crate::flags::reconcile_refusal_ignored(
                &format!("`reconcile = false` in the {}", file.display()),
                settings.authority,
            ));
        }
        // ⚠ There is deliberately NO twin here for `crate::flags::venue_catalog_refusal_ignored`,
        // and it is worth saying rather than leaving as an absence a reader has to notice.
        // `docs/decisions/0066`'s decision 4 describes the warn as firing "at every origin the
        // value can arrive from", listing four because that is where `reconcile`'s fires — but
        // `VIKE_DATAHUB_VENUE_CATALOG` was an ENVIRONMENT VARIABLE ONLY. It was never a
        // `FlagsPatch` key, so it can be neither a `flags.toml` line nor a row of this store, and a
        // check here would read a field that does not exist. `crate::load`'s environment-layer
        // check is the whole of it.
        settings.flags.apply(patch, &file)?;
    }
    Ok(())
}

/// **One section's assembled object, deserialized as its patch type.**
///
/// The object is a `serde_json::Value` every leaf of which was parsed by [`parse_row_value`], so
/// this is the TYPE check and nothing else — serde's own, the same `deny_unknown_fields` and the
/// same visitors a file line meets. MEASURED against the `toml` deserializer over the real four
/// patch types: the unknown-field message is character-identical (so
/// [`key_from_parse_message`]'s backtick markers extract the key unchanged), an integer still
/// deserializes into an `Option<f64>` (`max_notional_per_order = 100`, the live the CI box line), a
/// float still refuses an `Option<u64>` with serde's own wording, and an unknown enum variant still
/// names the legal set.
///
/// No redaction wrapper: unlike `toml::de::Error`, a `serde_json::Error` carries no input and its
/// `Display` renders no source snippet — see [`parse_row_value`].
///
/// ⚠ **The KEY is found in TWO ways, and the second exists because a `serde_json` deserializer does
/// not annotate.** A `toml::de::Error` appends its own *in `<key>`* tail to every message, so the
/// synthetic TOML document this replaced named a WRONG-TYPED row for free. Serde's own *unknown
/// field* message writes the name INSIDE itself, so [`key_from_parse_message`]'s markers still
/// catch the typo'd-key path unchanged — but a TYPE MISMATCH renders as a bare `invalid type:
/// string "30000", expected u64` with no name in it at all, and `ConfigError::Parse`'s
/// `key: Some(..)` arm became unreachable from here. MEASURED on one store, old path then new:
///
/// ```text
/// old: settings database (section `policy`): at line 1, column 26: invalid type: string "30000", expected u64; in `deadman_timeout_ms`
/// new: settings database (section `policy`): invalid type: string "30000", expected u64
/// ```
///
/// The section and the value, and not the row — which, to an operator holding twenty `policy` rows,
/// is the wrong tool. So when serde names no key, [`offending_leaf`] finds it by NARROWING: this
/// module's doc and the one on [`ConfigError`]'s own module both promise that a failure names the
/// key, and the promise is kept rather than downgraded to a residual.
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
/// "unmentioned = inherit" ([`PolicyPatch`] states the same rule for the loader). A
/// single-leaf object can therefore never fail for a MISSING field, so a leaf that refuses alone
/// refuses on its own merits.
///
/// Returns `None` when every leaf passes alone — the whole object then failed for something no one
/// row owns, and `key: None` is the honest answer. That is the pre-existing behaviour, kept as the
/// floor rather than replaced by a guess.
///
/// Only ever reached on the error path, so the cost is one deserialize per row of a section nobody
/// is going to boot with anyway.
fn offending_leaf<T: for<'de> Deserialize<'de>>(object: &serde_json::Value) -> Option<String> {
    json_leaves(object)
        .into_iter()
        .find(|(path, leaf)| T::deserialize(&nest(path, leaf)).is_err())
        .map(|(path, _)| path.join("."))
}

/// One LEAF of an assembled section: the path segments that reach it, and the value sitting there.
///
/// A named alias rather than the tuple spelled three times — it is [`json_leaves`]' return shape,
/// and [`nest`] is what puts one back together.
type Leaf<'a> = (Vec<String>, &'a serde_json::Value);

/// Every LEAF of an assembled section object, as its path segments and the value sitting there.
///
/// An EMPTY object is a leaf of its own — the same rule `crates/vike-config/tests/provenance.rs`'s
/// `walk` had to learn, and for the same reason: without it a table that is present and empty
/// contributes no path at all and the narrowing above would skip it silently.
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

#[cfg(test)]
mod tests {
    use super::*;

    fn dir_with(files: &[(&str, &str)]) -> tempfile::TempDir {
        let tmp = tempfile::tempdir().unwrap();
        for (name, body) in files {
            std::fs::write(tmp.path().join(name), body).unwrap();
        }
        tmp
    }

    /// ⚠ The three renderings this pins are the SAME BYTES a TOML rendering produced, and that is
    /// the point rather than a weakness: the values on both live boxes are in exactly this column,
    /// which is why the format change needs no second reader (see [`STALE_FORMAT_HINT`]).
    #[test]
    fn a_scalar_leaf_becomes_one_row_carrying_its_json_rendering() {
        let tmp = dir_with(&[
            (POLICY_FILE, "max_leverage = 3.0\n"),
            (PREFERENCES_FILE, "log_file_level = \"warn\"\n"),
            (FLAGS_FILE, "reconcile = true\n"),
        ]);
        let rows = rows_from_files(tmp.path()).unwrap();
        assert_eq!(
            rows.settings,
            vec![
                SettingRow {
                    section: "flags".into(),
                    key: "reconcile".into(),
                    value: "true".into()
                },
                SettingRow {
                    section: "policy".into(),
                    key: "max_leverage".into(),
                    value: "3.0".into()
                },
                SettingRow {
                    section: "preferences".into(),
                    key: "log_file_level".into(),
                    value: "\"warn\"".into()
                },
            ]
        );
        assert!(rows.arming.is_empty(), "no `[venues]` table was written, so none is mirrored");
    }

    /// The `[venues]` table becomes ROSTER-COMPLETE arming rows the moment the file states one —
    /// and stays absent when it does not. Both halves in one test, because the pair is the property.
    #[test]
    fn the_venues_table_mirrors_roster_complete_and_only_when_it_was_declared() {
        let silent = dir_with(&[(POLICY_FILE, "max_leverage = 1.0\n")]);
        assert!(rows_from_files(silent.path()).unwrap().arming.is_empty());

        let stated = dir_with(&[(POLICY_FILE, "[venues]\nbinance = \"demo\"\n")]);
        let rows = rows_from_files(stated.path()).unwrap();
        let venue_rows: Vec<_> = rows.arming.iter().filter(|r| r.label.is_none()).collect();
        assert_eq!(
            venue_rows.len(),
            vike_model::VENUES.len(),
            "a stated table is mirrored with a row per ROSTER venue, named even where the value \
             equals the fallback"
        );
        let binance = venue_rows.iter().find(|r| r.venue == "binance").unwrap();
        assert_eq!(binance.mode, "demo");
        assert!(
            venue_rows.iter().filter(|r| r.venue != "binance").all(|r| r.mode == "paper"),
            "every unnamed roster venue is written at its fallback, NAMED"
        );
    }

    #[test]
    fn an_accounts_table_becomes_labelled_arming_rows() {
        let tmp = dir_with(&[(
            POLICY_FILE,
            "[venues]\nhyperliquid = \"live\"\n\n[accounts.hyperliquid]\nALT = \"paper\"\n",
        )]);
        let rows = rows_from_files(tmp.path()).unwrap();
        let labelled: Vec<_> = rows.arming.iter().filter(|r| r.label.is_some()).collect();
        assert_eq!(
            labelled,
            vec![&ArmingRow {
                venue: "hyperliquid".into(),
                label: Some("ALT".into()),
                mode: "paper".into(),
                max_exposure: None
            }]
        );
    }

    /// The whole reason the rows go back through the patch types: a typo'd ROW KEY is refused BY
    /// NAME, exactly as a typo'd FILE key is, and the message names the store rather than a path an
    /// operator would open and find innocent.
    #[test]
    fn an_unknown_row_key_is_refused_by_name_on_the_read_path() {
        let store = StoredSettings {
            venue: Vec::new(),
            settings: vec![SettingRow {
                section: "policy".into(),
                key: "max_levrage".into(),
                value: "3.0".into(),
            }],
            arming: Vec::new(),
        };
        let mut settings = Settings::default();
        let err = apply_rows(&mut settings, &store).unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("max_levrage"), "the refusal must NAME the key: {msg}");
        assert!(
            msg.contains("settings database"),
            "…and the store it came from, not a file on disk: {msg}"
        );
    }

    /// A TOMBSTONE keeps its own message through the row path — `max_total_exposure` was DELETED
    /// from `Policy` for being unread, and an operator who writes it must be told where the concept
    /// went rather than "unknown field".
    #[test]
    fn a_tombstoned_key_keeps_its_own_refusal_through_the_rows() {
        let store = StoredSettings {
            venue: Vec::new(),
            settings: vec![SettingRow {
                section: "policy".into(),
                key: "max_total_exposure".into(),
                value: "500.0".into(),
            }],
            arming: Vec::new(),
        };
        let mut settings = Settings::default();
        let err = apply_rows(&mut settings, &store).unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("max_total_exposure"), "{msg}");
        assert!(
            !msg.contains("unknown field"),
            "a tombstone must not degrade to the generic unknown-key message: {msg}"
        );
    }

    /// A BOUND is imported from `vike-model`, and the row path gets it because it runs the same
    /// `apply`. No `CHECK` constraint restates it in SQL — that split-brain is what the typed model
    /// refuses.
    #[test]
    fn a_bound_still_bites_through_the_rows() {
        let store = StoredSettings {
            venue: Vec::new(),
            settings: vec![SettingRow {
                section: "policy".into(),
                key: "market_slippage".into(),
                value: "0.9".into(),
            }],
            arming: Vec::new(),
        };
        let mut settings = Settings::default();
        let err = apply_rows(&mut settings, &store).unwrap_err();
        assert!(err.to_string().contains("market_slippage"), "{err}");
    }

    /// An unknown VENUE id in the arming rows is refused too — `Policy::apply`'s hand-written
    /// roster check is the gate `deny_unknown_fields` cannot be, and it reaches the rows because
    /// the rows become that same `[venues]` table.
    #[test]
    fn an_unknown_venue_in_the_arming_rows_is_refused() {
        let store = StoredSettings {
            venue: Vec::new(),
            settings: Vec::new(),
            arming: vec![ArmingRow {
                venue: "not-a-venue".into(),
                label: None,
                mode: "live".into(),
                max_exposure: None,
            }],
        };
        let mut settings = Settings::default();
        let err = apply_rows(&mut settings, &store).unwrap_err();
        assert!(err.to_string().contains("not-a-venue"), "{err}");
    }

    /// An account LABEL round trips, and a label the alphabet refuses is still refused BY NAME.
    ///
    /// ⚠ **This test used to also measure a TOML bare-key QUOTER, and that function is gone with
    /// the document.** A JSON object key needs no quoting — any string is a legal key — so the
    /// rendering hazard the quoter defended against (an unquoted `format!` emitting an invalid
    /// document the day `vike_model::account_keys::AccountLabel`'s A-Z0-9 alphabet widens) cannot
    /// exist on this path at all. What survives is the REFUSAL, which was always the reachable half:
    /// `Policy::apply` rejects `my account` by name, and it still does through the rows.
    #[test]
    fn an_account_label_round_trips_and_a_refused_label_is_still_named() {
        let store = StoredSettings {
            venue: Vec::new(),
            settings: Vec::new(),
            arming: vec![ArmingRow {
                venue: "hyperliquid".into(),
                label: Some("ALT2".into()),
                mode: "paper".into(),
                max_exposure: None,
            }],
        };
        let mut settings = Settings::default();
        apply_rows(&mut settings, &store).unwrap();
        assert_eq!(
            settings.policy.venues.accounts_by_venue().get("hyperliquid").unwrap().get("ALT2"),
            Some(&crate::venue_mode::VenueMode::Paper)
        );

        // ...and a label the LABEL alphabet refuses is refused through the rows too, by name —
        // the same gate a `policy.toml` line hits, reached through the store.
        let refused = StoredSettings {
            venue: Vec::new(),
            settings: Vec::new(),
            arming: vec![ArmingRow {
                venue: "hyperliquid".into(),
                label: Some("my account".into()),
                mode: "paper".into(),
                max_exposure: None,
            }],
        };
        let err = apply_rows(&mut Settings::default(), &refused).unwrap_err();
        assert!(err.to_string().contains("my account"), "{err}");
    }

    /// An ARRAY leaf is refused rather than mangled. No key in the model has one today, so this
    /// drives the arm with a planted file — the alternative is an arm nothing ever executes.
    #[test]
    fn an_array_leaf_is_refused_by_name() {
        let file = Path::new("policy.toml");
        let mut out = Vec::new();
        let err = flatten(
            file,
            "policy",
            "somewhere",
            &toml::Value::Array(vec![toml::Value::Integer(1)]),
            &mut out,
        )
        .unwrap_err();
        assert!(err.to_string().contains("somewhere"), "{err}");
        assert!(out.is_empty(), "nothing is written on the refusal path");
    }

    /// **The two shapes TOML carries and JSON does not are REFUSED, not rendered.**
    ///
    /// Driven through [`flatten`] directly for the same reason the array test is: `load` refuses
    /// both before `rows_from_files` can reach them, so the alternative is an arm nothing executes.
    /// What this measures is the arm itself — and beside it, the exact mangling the obvious
    /// implementation (`serde_json::to_string` over the `toml::Value`) would have shipped, because
    /// "do not write it that way" is only a rule if the cost is on the record.
    #[test]
    fn a_datetime_and_a_non_finite_float_are_refused_rather_than_rendered() {
        let file = Path::new("policy.toml");
        let datetime = toml::Value::Datetime("1979-05-27T07:32:00Z".parse().unwrap());
        for (key, value) in [
            ("when", datetime.clone()),
            ("max_leverage", toml::Value::Float(f64::INFINITY)),
            ("max_leverage", toml::Value::Float(f64::NAN)),
        ] {
            let mut out = Vec::new();
            let err = flatten(file, "policy", key, &value, &mut out).unwrap_err();
            assert!(err.to_string().contains(key), "the refusal must NAME the key: {err}");
            assert!(out.is_empty(), "nothing is written on the refusal path");
            assert_eq!(json_scalar(&value), None, "…and the renderer agrees there is no form");
        }

        // The mangling this arm exists to prevent, MEASURED rather than asserted: the total
        // serializer answers for both, and both answers are wrong in a way nothing downstream can
        // see — `null` reads back as UNSET (a vanished ceiling), and the datetime reads back as a
        // MAP against a field expecting a scalar.
        assert_eq!(serde_json::to_string(&toml::Value::Float(f64::INFINITY)).unwrap(), "null");
        assert!(serde_json::to_string(&datetime).unwrap().contains("$__toml_private_datetime"));
    }

    /// **A malformed stored value is named AT ITS OWN ROW, by key** — the property the synthetic
    /// document was carried for, now bought by parsing each value on its own before it is placed.
    ///
    /// ⚠ **This test asserted a REFUSAL until 2026-09-18 and now asserts a DEGRADE, and the change
    /// is the point rather than a relaxation.** The first case is a STALE row: `'C:\vike\state'` is
    /// what the value column held for a Windows `config.state_dir` before it became JSON. Refusing
    /// it took every binary down on upgrade — `vike-cli config mirror` INCLUDED, the one command the
    /// refusal named — and `sqlite3` is installed on neither of this project's boxes. The dev box is
    /// Windows. [`RowRefusal`] carries the rule this now follows and `vike-boot` states it: *could
    /// this layer be read at all* DEGRADES, *does this layer say something illegal* REFUSES.
    ///
    /// What did NOT change is what the operator is told: the warning still names the row's key, the
    /// store, and the act that repairs it, and still never echoes the value.
    #[test]
    fn a_stale_row_degrades_by_name_rather_than_stopping_the_boot() {
        for (section, key, value) in [
            ("config", "state_dir", r"'C:\vike\state'"),
            ("preferences", "log_file_level", "\"\"\"warn\"\"\""),
            ("policy", "max_leverage", "inf"),
            // Trailing input after a complete scalar: the injection a text splice also allowed, and
            // the reason each value is parsed as ONE document rather than spliced into an object.
            ("policy", "max_leverage", "3.0, \"max_notional_per_order\": 9e9"),
        ] {
            let store = StoredSettings {
                venue: Vec::new(),
                settings: vec![SettingRow {
                    section: section.into(),
                    key: key.into(),
                    value: value.into(),
                }],
                arming: Vec::new(),
            };
            let mut settings = Settings::default();
            apply_rows(&mut settings, &store)
                .expect("an unreadable store degrades — the files still win, so it costs no value");
            let msg = settings
                .warnings
                .iter()
                .find(|w| w.contains("settings database was NOT read"))
                .unwrap_or_else(|| {
                    panic!("the degrade must WARN; warnings: {:?}", settings.warnings)
                })
                .clone();
            assert!(msg.contains(key), "the warning must NAME the row's key: {msg}");
            assert!(msg.contains("settings database"), "…and the store it came from: {msg}");
            assert!(
                msg.contains("config mirror"),
                "…and the act that re-derives the rows from the files: {msg}"
            );
            assert!(!msg.contains(r"C:\vike"), "the warning must not echo the value: {msg}");

            // …and the LAYER is dropped whole rather than half-applied: this store's one row was
            // the only thing it had to say, so nothing of it reached the resolved settings.
            assert_eq!(
                settings.policy.max_leverage,
                Settings::default().policy.max_leverage,
                "an unreadable store must contribute NOTHING, not its parseable half"
            );
        }
    }

    /// **An ILLEGAL row still refuses, and that half is what keeps the degrade above honest.**
    ///
    /// A value `serde_json` demoted from an integer to a float, because it fits neither `i64` nor
    /// `u64`. The synthetic TOML document refused it; without this arm it resolves to an enormous
    /// finite number, passes `check_positive`, and leaves a per-order CEILING effectively absent
    /// while `config show` reports it resolved from `db`. No mirror can write one — `json_scalar`
    /// renders integers from an `i64` — so it is exactly the hand-`INSERT` class `null` is refused
    /// for, and it must not ride the stale-format degrade out of the building.
    #[test]
    fn an_integer_too_large_to_be_one_is_refused_rather_than_read_as_a_float() {
        let store = StoredSettings {
            venue: Vec::new(),
            settings: vec![SettingRow {
                section: "policy".into(),
                key: "max_notional_per_order".into(),
                value: "18446744073709551616".into(),
            }],
            arming: Vec::new(),
        };
        let mut settings = Settings::default();
        let err = apply_rows(&mut settings, &store)
            .expect_err("an integer outside the store's range is ILLEGAL, not merely unreadable");
        let msg = err.to_string();
        assert!(msg.contains("max_notional_per_order"), "the refusal must NAME the key: {msg}");
        assert!(
            msg.contains("INTEGER outside the range"),
            "…and say what is wrong with it rather than wearing the stale-format hint: {msg}"
        );
        assert!(
            settings.warnings.is_empty(),
            "an illegal row must REFUSE, never warn-and-continue: {:?}",
            settings.warnings
        );
    }

    /// **A `null` row is refused rather than read as "unset".** JSON's one expressive advantage
    /// over TOML is the one thing this column must not accept: `deny_unknown_fields` sees a KNOWN
    /// field name, and every patch field is an `Option`, so `null` would deserialize as absence and
    /// a ceiling an operator filed would resolve to its default in silence.
    #[test]
    fn a_null_row_value_is_refused_rather_than_read_as_unset() {
        let store = StoredSettings {
            venue: Vec::new(),
            settings: vec![SettingRow {
                section: "policy".into(),
                key: "max_notional_per_order".into(),
                value: "null".into(),
            }],
            arming: Vec::new(),
        };
        let err = apply_rows(&mut Settings::default(), &store).unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("max_notional_per_order"), "{msg}");
        assert!(msg.contains("null"), "{msg}");

        // ...and the refusal is doing real work: serde itself accepts it, silently, as `None`.
        let object = serde_json::json!({ "max_notional_per_order": null });
        let patch: PolicyPatch = serde_json::from_value(object).unwrap();
        assert!(patch.max_notional_per_order.is_none(), "which is exactly the silent read above");
    }

    /// A key and a path THROUGH that key cannot both be set. The TOML document this replaced got
    /// the refusal from its parser; `UNIQUE (section, key)` cannot see it, so it is checked by hand.
    #[test]
    fn two_rows_claiming_one_place_are_refused_rather_than_one_winning() {
        let store = StoredSettings {
            venue: Vec::new(),
            settings: vec![
                SettingRow { section: "policy".into(), key: "rate".into(), value: "1".into() },
                SettingRow {
                    section: "policy".into(),
                    key: "rate.max_utilization".into(),
                    value: "0.5".into(),
                },
            ],
            arming: Vec::new(),
        };
        let err = apply_rows(&mut Settings::default(), &store).unwrap_err();
        assert!(err.to_string().contains("rate"), "{err}");
    }

    /// A dotted row still becomes a NESTED object — the shape `PolicyPatch::rate` expects — so the
    /// tombstone it is guarding reaches its own message rather than "unknown field".
    #[test]
    fn a_dotted_row_key_becomes_a_nested_object() {
        let store = StoredSettings {
            venue: Vec::new(),
            settings: vec![SettingRow {
                section: "policy".into(),
                key: "rate.max_utilization".into(),
                value: "0.5".into(),
            }],
            arming: Vec::new(),
        };
        let values = section_values(&store).unwrap();
        assert_eq!(values["policy"], serde_json::json!({ "rate": { "max_utilization": 0.5 } }));
    }

    /// **A finite `f64` survives the JSON round trip BIT-EXACTLY, and so does the rendering this
    /// replaced** — the two halves a ceiling depends on, swept rather than sampled.
    ///
    /// 20 000 random bit patterns through a deterministic LCG, so the sweep is the same every run
    /// and a failure is reproducible from the seed. `serde_json` is pinned with `float_roundtrip`
    /// at the root manifest (the parse is correctly rounded) and renders with `ryu` (the shortest
    /// string that reads back as the same bits); the second half asks the migration question of the
    /// same values — a `toml::Value`'s own `Display` is a long fixed-point string for a subnormal,
    /// and it must still read back as the SAME number rather than a nearby one.
    #[test]
    fn a_finite_f64_survives_the_json_round_trip_bit_exactly() {
        let mut state: u64 = 0x2026_0918_0057;
        let mut swept = 0usize;
        for _ in 0..20_000 {
            state = state.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            let x = f64::from_bits(state);
            if !x.is_finite() {
                continue;
            }
            swept += 1;

            let rendered = json_scalar(&toml::Value::Float(x)).expect("a finite float renders");
            let back: f64 = serde_json::from_str(&rendered).expect("…and parses back");
            assert_eq!(back.to_bits(), x.to_bits(), "{x:?} rendered as {rendered}");

            // The migration half: the bytes the column held BEFORE, read by the parser it holds now.
            let old = toml::Value::Float(x).to_string();
            let from_old: f64 = serde_json::from_str(&old)
                .unwrap_or_else(|e| panic!("the old rendering {old} is not JSON: {e}"));
            assert_eq!(from_old.to_bits(), x.to_bits(), "{x:?} stored as {old}");
        }
        assert!(swept > 15_000, "the sweep must reach real values, not mostly NaN: {swept}");
    }

    /// The arming rows become the two MAPS `PolicyPatch` expects, placed whole.
    #[test]
    fn the_arming_rows_become_the_two_policy_maps() {
        let store = StoredSettings {
            venue: Vec::new(),
            settings: Vec::new(),
            arming: vec![
                ArmingRow {
                    venue: "binance".into(),
                    label: None,
                    mode: "demo".into(),
                    max_exposure: None,
                },
                ArmingRow {
                    venue: "hyperliquid".into(),
                    label: Some("ALT".into()),
                    mode: "paper".into(),
                    max_exposure: None,
                },
            ],
        };
        let values = section_values(&store).unwrap();
        assert_eq!(
            values["policy"],
            serde_json::json!({
                "venues": { "binance": "demo" },
                "accounts": { "hyperliquid": { "ALT": "paper" } },
            })
        );
    }
}
