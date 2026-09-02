//! The gate on `vike_config::provenance` — the TOML half of `vike-cli config show`.
//!
//! Two properties, and the first is the one that matters:
//!
//! 1. **Every settings field has a provenance row.** [`every_settings_field_has_a_provenance_row`]
//!    walks the REAL serialized [`vike_config::Settings`] — not a list written by hand — and demands
//!    a `setting_keys()` row for every leaf it finds, and no row for a leaf that does not exist. A
//!    field added to `Policy`/`Config`/`Preferences`/`Flags` therefore fails HERE, in the crate that
//!    owns it, rather than silently going missing from the only command that discloses settings.
//!    That is the failure mode the whole feature exists to remove: a ceiling nobody can see is a
//!    ceiling nobody can confirm.
//!
//!    ⚠ It walks `serde_json`, not `toml`, deliberately: TOML has no null, so an `Option::None`
//!    field VANISHES from a TOML table and would be invisible to this gate — the exact drift it
//!    exists to catch. `serde_json` renders it `null` and the leaf is still there to demand a row.
//!
//! 2. **Origin is measured, not inferred.** The rest of the tests drive [`vike_config::describe`]
//!    over real files in a `TempDir` and assert the reported layer against what was actually
//!    written: presence in a file beats the default even when the value EQUALS the default, the
//!    environment beats a file, and no environment on earth can move a `Policy` row off
//!    `policy.toml`.

use std::collections::{BTreeSet, HashMap};
use std::path::Path;

use vike_config::provenance::{describe, setting_keys, Origin};
use vike_config::{
    load::{CONFIG_FILE, FLAGS_FILE, POLICY_FILE, PREFERENCES_FILE},
    Settings,
};

// ------------------------------------------------------------------------------------------------
// Helpers
// ------------------------------------------------------------------------------------------------

fn env(pairs: &[(&str, &str)]) -> HashMap<String, String> {
    pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect()
}

fn write(dir: &Path, name: &str, body: &str) {
    std::fs::write(dir.join(name), body).expect("write settings file");
}

/// The row for one dotted key, or a panic naming it — a missing row is always a bug in the table,
/// never a legitimate outcome, and `unwrap()` alone would not say which key.
fn row(rows: &[vike_config::ResolvedSetting], key: &str) -> vike_config::ResolvedSetting {
    rows.iter()
        .find(|r| r.key == key)
        .unwrap_or_else(|| panic!("no provenance row for {key}"))
        .clone()
}

/// Every leaf path of a serialized [`Settings`], dotted — `warnings` excluded, since it is the
/// loader's own output rather than a setting anybody configures.
fn settings_leaves() -> BTreeSet<String> {
    fn walk(prefix: &str, value: &serde_json::Value, out: &mut BTreeSet<String>) {
        match value {
            // ⚠ An EMPTY object is a leaf of its own, and that is a rule this walk did NOT have.
            // Without it, a settings field whose default is an empty MAP contributes nothing at all
            // and escapes the completeness gate entirely — it can be declared, validated, accepted
            // by `deny_unknown_fields` and read by `config show`, and neither half of
            // `every_settings_field_has_a_provenance_row` would ever mention it. `policy.accounts`
            // is exactly that field: its default is empty on every box that has not written an
            // `[accounts]` table, which today is nearly all of them. Recursing into a non-empty
            // object is unchanged, so no existing leaf moves.
            serde_json::Value::Object(map) if map.is_empty() => {
                out.insert(prefix.to_string());
            }
            serde_json::Value::Object(map) => {
                for (k, v) in map {
                    let key = if prefix.is_empty() { k.clone() } else { format!("{prefix}.{k}") };
                    walk(&key, v, out);
                }
            }
            _ => {
                out.insert(prefix.to_string());
            }
        }
    }
    let value = serde_json::to_value(Settings::default()).expect("Settings serializes");
    let mut out = BTreeSet::new();
    walk("", &value, &mut out);
    out.retain(|k| k != "warnings");
    out
}

// ------------------------------------------------------------------------------------------------
// 1. Completeness — the gate that bites
// ------------------------------------------------------------------------------------------------

#[test]
fn every_settings_field_has_a_provenance_row() {
    let leaves = settings_leaves();
    let declared: BTreeSet<String> = setting_keys().into_iter().map(|k| k.key).collect();

    let missing: Vec<&String> = leaves.difference(&declared).collect();
    assert!(
        missing.is_empty(),
        "\n{} settings field(s) have NO provenance row, so `vike-cli config show` would not\n\
         disclose them at all:\n\n{}\n\n\
         FIX: add a row to `vike_config::provenance::setting_keys` — the dotted key, the file it\n\
         lives in, its path INSIDE that file (no section prefix), and every environment variable\n\
         `crate::layers` lets override it (an empty list for a `Policy` key, which by construction\n\
         has none). A flag needs no row here: the flags half is derived from FLAG_REGISTRY.\n",
        missing.len(),
        missing.iter().map(|k| format!("  {k}")).collect::<Vec<_>>().join("\n"),
    );

    let stale: Vec<&String> = declared.difference(&leaves).collect();
    assert!(
        stale.is_empty(),
        "\nprovenance rows for field(s) that no longer exist on `Settings`:\n\n{}\n\n\
         FIX: delete the row — a one-line cleanup, never a blocker.\n",
        stale.iter().map(|k| format!("  {k}")).collect::<Vec<_>>().join("\n"),
    );
}

/// The gate above is only as good as its INPUT. A `settings_leaves()` that silently started
/// returning nothing would make it vacuously green — the "mutation-test every gate" rule.
#[test]
fn the_completeness_gate_has_a_non_empty_input() {
    let leaves = settings_leaves();
    assert!(leaves.len() > 30, "only {} leaves — the walk is broken: {leaves:?}", leaves.len());
    for expected in [
        "policy.max_leverage",
        "policy.max_notional_per_order", // an Option::None — the leaf `toml` would have lost
        "policy.market_slippage",
        // The empty-map leaf the walk's first arm exists for — absent from this list, the arm
        // could be deleted and this anti-vacuity guard would still pass.
        "policy.accounts",
        "config.datahub_addr",
        "preferences.log_file_level",
        "flags.reconcile",
    ] {
        assert!(leaves.contains(expected), "{expected} missing from {leaves:?}");
    }
    assert!(!leaves.contains("warnings"), "the loader's own output is not a setting");
}

// ------------------------------------------------------------------------------------------------
// 2. Origin is measured, not inferred
// ------------------------------------------------------------------------------------------------

/// No settings directory at all: every row reports `default`, every file reports absent. This is
/// the shape of the failure the feature exists to make visible — a binary that resolved no project
/// and is therefore running with NO ceiling, which used to look identical to a configured one.
#[test]
fn no_settings_directory_means_every_row_is_a_default() {
    let d = describe(None, &env(&[])).unwrap();
    assert!(d.settings_dir.is_none());
    assert!(d.files.iter().all(|f| !f.present && f.keys == 0), "{:?}", d.files);
    assert!(d.rows.iter().all(|r| r.origin == Origin::Default), "a row claimed a layer");
    assert_eq!(row(&d.rows, "policy.max_notional_per_order").value, None, "no ceiling is armed");
}

/// **The central property.** A key present in `policy.toml` reports `policy.toml` EVEN WHEN its
/// value equals the code default. A value-diff implementation would report `default` here and tell
/// the operator their file did nothing — which is the lie this module exists not to tell.
#[test]
fn presence_in_a_file_beats_the_default_even_at_the_default_value() {
    let dir = tempfile::tempdir().unwrap();
    write(dir.path(), POLICY_FILE, "max_leverage = 1.0\n");

    let d = describe(Some(dir.path()), &env(&[])).unwrap();
    let r = row(&d.rows, "policy.max_leverage");
    assert_eq!(r.origin, Origin::File(POLICY_FILE));
    assert_eq!(r.value.as_deref(), Some("1.0"));
    assert_eq!(r.default.as_deref(), Some("1.0"), "the default column still states the default");
    assert!(!r.adjusted, "1.0 and 1.0 are the same value");

    let policy = d.files.iter().find(|f| f.name == POLICY_FILE).unwrap();
    assert!(policy.present);
    assert_eq!(policy.keys, 1);
    assert_eq!(policy.path, dir.path().join(POLICY_FILE));
}

/// `max_leverage = 3` is a TOML integer and the effective value is the float `3.0`. Reporting that
/// as an adjustment would be a formatting artifact reported as a fact.
#[test]
fn an_integer_in_a_file_is_not_an_adjustment_of_its_float() {
    let dir = tempfile::tempdir().unwrap();
    write(dir.path(), POLICY_FILE, "max_leverage = 3\n");
    let d = describe(Some(dir.path()), &env(&[])).unwrap();
    let r = row(&d.rows, "policy.max_leverage");
    assert_eq!(r.origin, Origin::File(POLICY_FILE));
    assert!(!r.adjusted, "{r:?}");
}

/// ⚠ `a_clamped_preference_is_flagged_as_adjusted` stood here. It drove the model's ONLY
/// policy-binds-preference clamp (`policy.rate.max_utilization` over
/// `preferences.rate_utilization`), and both of those fields were removed as a ceiling bounding a
/// value nothing read. No clamp exists, so nothing can be `adjusted` and there is no honest way to
/// exercise the flag through `describe` — a test that hand-built a `ResolvedSetting` would assert
/// only that a struct literal holds what was put in it.
///
/// What replaces it is the property that IS true now, and it is the one an operator depends on: an
/// unadjusted row must not claim to be adjusted, so the flag never fires spuriously while there is
/// nothing to adjust.
#[test]
fn no_row_is_reported_as_adjusted_while_no_clamp_exists() {
    let dir = tempfile::tempdir().unwrap();
    write(dir.path(), POLICY_FILE, "max_leverage = 3.0\n");
    write(dir.path(), PREFERENCES_FILE, "log_level = \"warn\"\n");

    let d = describe(Some(dir.path()), &env(&[])).unwrap();
    assert!(d.settings.warnings.is_empty(), "{:?}", d.settings.warnings);
    for r in &d.rows {
        assert!(!r.adjusted, "nothing adjusts a value today, but {r:?} claims otherwise");
    }
    // …and the file-set rows still report the file as their origin, verbatim.
    let r = row(&d.rows, "preferences.log_level");
    assert_eq!(r.origin, Origin::File(PREFERENCES_FILE));
    assert_eq!(r.origin_value.as_deref(), Some("warn"));
    assert_eq!(r.value.as_deref(), Some("warn"));
}

/// The environment beats a file for the three types that HAVE an env layer, and the reported origin
/// names the variable that matched — not merely "env".
#[test]
fn the_environment_beats_a_file_and_names_the_variable() {
    let dir = tempfile::tempdir().unwrap();
    write(dir.path(), CONFIG_FILE, "log_dir = \"/from/file\"\n");
    write(dir.path(), FLAGS_FILE, "reconcile = false\n");

    let d =
        describe(Some(dir.path()), &env(&[("VIKE_LOG_DIR", "/from/env"), ("VIKE_RECONCILE", "1")]))
            .unwrap();

    let log_dir = row(&d.rows, "config.log_dir");
    assert_eq!(log_dir.origin, Origin::Env("VIKE_LOG_DIR"));
    assert_eq!(log_dir.value.as_deref(), Some("/from/env"));

    let reconcile = row(&d.rows, "flags.reconcile");
    assert_eq!(reconcile.origin, Origin::Env("VIKE_RECONCILE"));
    assert_eq!(reconcile.value.as_deref(), Some("true"));
}

/// `RUST_LOG` wins over its `VIKE_LOG` alias, and the row says WHICH — the whole reason
/// [`Origin::Env`] carries a name instead of being one flat word.
#[test]
fn the_alias_precedence_is_reported_by_name() {
    let both = env(&[("RUST_LOG", "debug"), ("VIKE_LOG", "trace")]);
    let d = describe(None, &both).unwrap();
    assert_eq!(row(&d.rows, "preferences.log_level").origin, Origin::Env("RUST_LOG"));

    let alias = env(&[("VIKE_LOG", "trace")]);
    let d = describe(None, &alias).unwrap();
    let r = row(&d.rows, "preferences.log_level");
    assert_eq!(r.origin, Origin::Env("VIKE_LOG"));
    assert_eq!(r.value.as_deref(), Some("trace"));
}

/// An exported `NAME=` configures nothing (`layers::get` treats empty as unset), and the row must
/// agree with the loader rather than claiming an override that did not happen.
#[test]
fn an_empty_variable_does_not_claim_an_override() {
    let dir = tempfile::tempdir().unwrap();
    write(dir.path(), CONFIG_FILE, "log_dir = \"/from/file\"\n");
    let d = describe(Some(dir.path()), &env(&[("VIKE_LOG_DIR", "")])).unwrap();
    let r = row(&d.rows, "config.log_dir");
    assert_eq!(r.origin, Origin::File(CONFIG_FILE));
    assert_eq!(r.value.as_deref(), Some("/from/file"));
}

/// **A ceiling has no environment layer, and the description must never suggest it does.** The two
/// variables that USED to raise this exact ceiling (both REMOVED in Phase 5, both refused at
/// startup) are exported here alongside a populated environment; every policy row must still report
/// the file, or the default.
///
/// Only already-declared variable names appear here on purpose: `crates/vike-ops/tests/
/// settings_registry.rs` harvests every env-shaped string literal under `crates/`, so an invented
/// `VIKE_*` name in a test would fail that gate as an undeclared read.
#[test]
fn nothing_in_the_environment_can_move_a_policy_row() {
    let dir = tempfile::tempdir().unwrap();
    write(dir.path(), POLICY_FILE, "max_notional_per_order = 250\n");

    let hostile = env(&[
        ("VIKE_MAX_ORDER_NOTIONAL", "999999"),
        ("VIKE_TRADEHUB_MAX_ORDER_NOTIONAL", "999999"),
        ("VIKE_RECONCILE", "1"),
        ("VIKE_LOG_DIR", "/somewhere"),
    ]);
    let d = describe(Some(dir.path()), &hostile).unwrap();

    let capped = row(&d.rows, "policy.max_notional_per_order");
    assert_eq!(capped.origin, Origin::File(POLICY_FILE));
    assert_eq!(capped.value.as_deref(), Some("250.0"), "the EFFECTIVE f64, not the file text");

    for r in d.rows.iter().filter(|r| r.file == POLICY_FILE) {
        assert!(
            matches!(r.origin, Origin::File(_) | Origin::Default),
            "{} claims {:?} — a policy ceiling has no env layer",
            r.key,
            r.origin
        );
    }
}

/// **The disclosure command refuses whatever the daemons refuse.**
///
/// `describe` calls `load`, so a `<project>/vike.toml` — the removed per-project override layer —
/// fails HERE too rather than being described as absent. That matters more than it looks: the one
/// command whose job is answering *which files did you read?* must not answer it for a loader
/// different from the one a daemon runs, which is the exact combination that let the layer be
/// advertised-but-unread. And the four rows it does list are exactly the files the loader reads.
#[test]
fn a_removed_project_file_fails_the_description_and_never_becomes_a_row() {
    let project = tempfile::tempdir().unwrap();
    let dir = project.path().join("settings");
    std::fs::create_dir(&dir).unwrap();
    write(&dir, CONFIG_FILE, "log_dir = \"/from/file\"\n");

    // Absent: four file rows, and the settings file is the origin.
    let d = describe(Some(&dir), &env(&[])).unwrap();
    assert_eq!(d.files.len(), 4, "{:?}", d.files.iter().map(|f| f.name).collect::<Vec<_>>());
    assert!(!d.files.iter().any(|f| f.name.contains("vike.toml")), "{:?}", d.files);
    assert_eq!(row(&d.rows, "config.log_dir").origin, Origin::File(CONFIG_FILE));

    // Present: refused, naming the file.
    std::fs::write(project.path().join("vike.toml"), "[config]\nlog_dir = \"/from/project\"\n")
        .unwrap();
    let err = describe(Some(&dir), &env(&[])).unwrap_err().to_string();
    assert!(err.contains("vike.toml"), "{err}");
}

/// The kill switch arms on PRESENCE, whatever the value — including an empty one, which every other
/// variable treats as unset. The description must report the same rule the loader applies.
#[test]
fn the_presence_armed_kill_switch_is_reported_from_an_empty_value() {
    let d = describe(None, &env(&[("POLY_REDEEM_HALT", "")])).unwrap();
    let r = row(&d.rows, "flags.poly_redeem_halt");
    assert_eq!(r.origin, Origin::Env("POLY_REDEEM_HALT"));
    assert_eq!(r.value.as_deref(), Some("true"));
}

/// `describe` calls `load`, so a broken file fails HERE too, naming the file — a description that
/// quietly reported defaults over an unparseable `policy.toml` would be the worst output of all.
#[test]
fn a_broken_settings_file_is_an_error_not_a_silent_default() {
    let dir = tempfile::tempdir().unwrap();
    write(dir.path(), POLICY_FILE, "max_leverage = \"not a number\"\n");
    let err = describe(Some(dir.path()), &env(&[])).unwrap_err().to_string();
    assert!(err.contains(POLICY_FILE), "{err}");
}

/// The description and the loader can never disagree: the `Settings` it returns is the one `load`
/// produced from the same two arguments.
#[test]
fn the_description_carries_the_settings_the_loader_resolved() {
    let dir = tempfile::tempdir().unwrap();
    write(dir.path(), POLICY_FILE, "max_notional_per_order = 250\n");
    write(dir.path(), FLAGS_FILE, "poly_exec = true\n");
    let vars = env(&[("VIKE_RECONCILE", "1")]);

    let d = describe(Some(dir.path()), &vars).unwrap();
    assert_eq!(d.settings, vike_config::load(Some(dir.path()), &vars).unwrap());
    assert_eq!(d.settings.policy.max_notional_per_order, Some(250.0));
}
