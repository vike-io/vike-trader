//! The gate on `vike_config::provenance` — the settings-row half of `vike-cli config show`.
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
//! 2. **Origin is measured, not inferred.** The rest of the tests drive
//!    [`vike_config::describe_with_source`] over hand-built settings ROWS and assert the reported
//!    layer against what was actually stored: presence in the store beats the default even when the
//!    value EQUALS the default, and there is no third layer (decision 0111 removed the environment
//!    one), so every row is `db` or `default`.
//!
//! `describe`/`describe_with_source` read no settings file (`docs/decisions/0086`), so every
//! fixture below is built as rows, resolved through `describe_with_source`; `Origin::Db` is the
//! ordinary non-default answer.

use std::assert_matches;
use std::collections::BTreeSet;

use vike_config::provenance::setting_keys;
use vike_config::{Origin, Settings, StoreLayer, describe, describe_with_source};
use vike_secrets::{SettingRow, StoredSettings};

// ------------------------------------------------------------------------------------------------
// Helpers
// ------------------------------------------------------------------------------------------------

/// The row for one dotted key, or a panic naming it — a missing row is always a bug in the table,
/// never a legitimate outcome, and `unwrap()` alone would not say which key.
fn row(rows: &[vike_config::ResolvedSetting], key: &str) -> vike_config::ResolvedSetting {
    rows.iter()
        .find(|r| r.key == key)
        .unwrap_or_else(|| panic!("no provenance row for {key}"))
        .clone()
}

/// Every leaf path of a serialized [`Settings`], dotted — the loader's own RESOLUTION-REPORT
/// fields excluded, since they are what the load produced rather than settings anybody configures.
fn settings_leaves() -> BTreeSet<String> {
    fn walk(prefix: &str, value: &serde_json::Value, out: &mut BTreeSet<String>) {
        match value {
            // ⚠ An EMPTY object is a leaf of its own. Without this arm, a settings field whose
            // default is an empty MAP contributes nothing at all and escapes the completeness gate
            // entirely — it can be declared, validated, accepted by `deny_unknown_fields` and read
            // by `config show`, and neither half of `every_settings_field_has_a_provenance_row`
            // would ever mention it. No field has that shape today (the last one was the
            // per-account arming map, gone with the account table taking its place), so the arm is
            // a guard for the next one. Recursing into a non-empty object is unchanged.
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
    // The RESOLUTION-REPORT fields, which are not settings anybody configures: they are what the
    // loader RESOLVED, and none of them can be written in the environment or as a row.
    // `warnings` is the loader's own non-fatal output; `store_refusal` is set when the store could
    // not be opened at all; `seal_refusal` when it opened and said something ILLEGAL — a seal whose
    // counts do not match its tables, or a row that will not parse. The last pair are deliberately
    // two fields and not one: they differ in what the OPERATOR must do, and `config show` renders
    // them differently.
    //
    // ⚠ Excluded BY NAME rather than by a shape rule, and spelled out one per line, because this
    // list is the one place this gate can be silently widened. A `Settings` field that is genuinely
    // a SETTING and gets added here escapes the completeness check entirely — which is the exact
    // drift the gate exists to catch, wearing the gate's own clothes. The test that an exclusion is
    // legitimate: could an operator write this key in the environment or as a `setting` row? If yes
    // it needs a provenance row, not a line here.
    out.retain(|k| k != "warnings" && k != "store_refusal" && k != "seal_refusal");
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
         FIX: add a row to `vike_config::provenance::setting_keys` — the dotted key, the section it\n\
         lives in, its path INSIDE that section (no section prefix), and every environment variable\n\
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

/// No settings directory at all: every row reports `default`. This is the shape of the failure the
/// feature exists to make visible — a binary that resolved no project and is therefore running with
/// NO ceiling, which used to look identical to a configured one.
#[test]
fn no_settings_directory_means_every_row_is_a_default() {
    let d = describe(None).unwrap();
    assert!(d.settings_dir.is_none());
    assert!(d.rows.iter().all(|r| r.origin == Origin::Default), "a row claimed a layer");
    assert_eq!(row(&d.rows, "policy.max_notional_per_order").value, None, "no ceiling is armed");
}

/// **The central property.** A key present in the settings ROWS reports `db` EVEN WHEN its value
/// equals the code default. A value-diff implementation would report `default` here and tell the
/// operator their write did nothing — which is the lie this module exists not to tell.
#[test]
fn presence_in_the_store_beats_the_default_even_at_the_default_value() {
    let dir = tempfile::tempdir().unwrap();
    let rows = StoredSettings {
        settings: vec![SettingRow {
            section: "policy".into(),
            key: "max_leverage".into(),
            value: "1.0".into(),
        }],
        ..Default::default()
    };

    let d = describe_with_source(Some(dir.path()), StoreLayer::Rows { rows: &rows, adopted: None })
        .unwrap();
    let r = row(&d.rows, "policy.max_leverage");
    assert_eq!(r.origin, Origin::Db);
    assert_eq!(r.value.as_deref(), Some("1.0"));
    assert_eq!(r.default.as_deref(), Some("1.0"), "the default column still states the default");
    assert!(!r.adjusted, "1.0 and 1.0 are the same value");
}

/// A row storing `3` is a JSON integer and the effective value is the float `3.0`. Reporting that as
/// an adjustment would be a formatting artifact reported as a fact.
#[test]
fn an_integer_row_is_not_an_adjustment_of_its_float() {
    let dir = tempfile::tempdir().unwrap();
    let rows = StoredSettings {
        settings: vec![SettingRow {
            section: "policy".into(),
            key: "max_leverage".into(),
            value: "3".into(),
        }],
        ..Default::default()
    };
    let d = describe_with_source(Some(dir.path()), StoreLayer::Rows { rows: &rows, adopted: None })
        .unwrap();
    let r = row(&d.rows, "policy.max_leverage");
    assert_eq!(r.origin, Origin::Db);
    assert!(!r.adjusted, "{r:?}");
}

/// ⚠ No policy-binds-preference clamp exists (`a_clamped_preference_is_flagged_as_adjusted` went
/// with the last one), so nothing can be `adjusted` and there is no honest way to exercise the flag
/// through `describe_with_source` — a test that hand-built a `ResolvedSetting` would assert only
/// that a struct literal holds what was put in it.
///
/// What is tested is the property an operator depends on: an
/// unadjusted row must not claim to be adjusted, so the flag never fires spuriously while there is
/// nothing to adjust.
#[test]
fn no_row_is_reported_as_adjusted_while_no_clamp_exists() {
    let dir = tempfile::tempdir().unwrap();
    let rows = StoredSettings {
        settings: vec![
            SettingRow {
                section: "policy".into(),
                key: "max_leverage".into(),
                value: "3.0".into(),
            },
            SettingRow {
                section: "preferences".into(),
                key: "log_level".into(),
                value: "\"warn\"".into(),
            },
        ],
        ..Default::default()
    };
    let d = describe_with_source(Some(dir.path()), StoreLayer::Rows { rows: &rows, adopted: None })
        .unwrap();
    assert!(d.settings.warnings.is_empty(), "{:?}", d.settings.warnings);

    // ⚠ Without these two the loop below is vacuous: a row that resolved to `default` can never be
    // `adjusted`, so the layer has to be proven LIVE before its flag means anything.
    assert_eq!(row(&d.rows, "policy.max_leverage").origin, Origin::Db);
    assert_eq!(row(&d.rows, "preferences.log_level").origin, Origin::Db);
    for r in &d.rows {
        assert!(!r.adjusted, "nothing adjusts a value today, but {r:?} claims otherwise");
    }
}

/// **No row reports an environment origin** (decision 0111): there is no environment layer, so the
/// only answers are the store and the default — a ceiling and a flag alike.
#[test]
fn every_row_is_the_store_or_the_default() {
    let dir = tempfile::tempdir().unwrap();
    let rows = StoredSettings {
        settings: vec![
            SettingRow {
                section: "policy".into(),
                key: "max_notional_per_order".into(),
                value: "250".into(),
            },
            SettingRow {
                section: "config".into(),
                key: "log_dir".into(),
                value: "\"/from/db\"".into(),
            },
            SettingRow { section: "flags".into(), key: "reconcile".into(), value: "true".into() },
        ],
        ..Default::default()
    };
    let d = describe_with_source(Some(dir.path()), StoreLayer::Rows { rows: &rows, adopted: None })
        .unwrap();

    let capped = row(&d.rows, "policy.max_notional_per_order");
    assert_eq!(capped.origin, Origin::Db);
    assert_eq!(capped.value.as_deref(), Some("250.0"), "the EFFECTIVE f64, not the row text");
    assert_eq!(row(&d.rows, "config.log_dir").origin, Origin::Db);
    assert_eq!(row(&d.rows, "flags.reconcile").origin, Origin::Db);
    for r in &d.rows {
        assert_matches!(
            r.origin,
            Origin::Db | Origin::Default,
            "{} claims {:?} — there is no other layer",
            r.key,
            r.origin
        );
    }
}

/// **The disclosure command refuses whatever the daemons refuse.**
///
/// `describe` calls `load`, so a `<project>/vike.toml` — the removed per-project override layer —
/// fails HERE too rather than being described as absent. That matters more than it looks: the one
/// command whose job is answering *what is in force?* must not answer it for a loader different from
/// the one a daemon runs, which is the exact combination that let the layer be advertised-but-unread.
#[test]
fn a_removed_project_file_fails_the_description() {
    let project = tempfile::tempdir().unwrap();
    let dir = project.path().join("settings");
    std::fs::create_dir(&dir).unwrap();

    // Absent: an ordinary description.
    let d = describe(Some(&dir)).unwrap();
    assert!(d.settings_dir.is_some());

    // Present: refused, naming the file.
    std::fs::write(project.path().join("vike.toml"), "[config]\nlog_dir = \"/from/project\"\n")
        .unwrap();
    let err = describe(Some(&dir)).unwrap_err().to_string();
    assert!(err.contains("vike.toml"), "{err}");
}

/// **A row that will not parse is MARKED rather than silently defaulted, and `describe_with_source`
/// survives it** — the disclosure verb an operator reaches for precisely when a box will not start
/// must not brick over the same finding.
///
/// A bad ROW does not error — see `crate::mirror::apply_rows`'s own doc for why (this replaced
/// `a_broken_settings_file_is_an_error_not_a_silent_default`). The description comes back, with the
/// mark on `Settings::seal_refusal` an operator needs.
#[test]
fn an_illegal_row_marks_the_description_rather_than_erroring() {
    let dir = tempfile::tempdir().unwrap();
    let rows = StoredSettings {
        settings: vec![SettingRow {
            section: "policy".into(),
            key: "max_leverage".into(),
            value: "\"not a number\"".into(),
        }],
        ..Default::default()
    };
    let d = describe_with_source(Some(dir.path()), StoreLayer::Rows { rows: &rows, adopted: None })
        .expect("an illegal row must not take the disclosure verb down with it");
    let why = d.settings.seal_refusal.expect("an illegal row must be marked");
    assert!(why.contains("max_leverage"), "{why}");
}

/// The description and the loader can never disagree: the `Settings` it returns is the one
/// `load_with_source` produced from the same arguments.
#[test]
fn the_description_carries_the_settings_the_loader_resolved() {
    let dir = tempfile::tempdir().unwrap();
    let rows = StoredSettings {
        settings: vec![
            SettingRow {
                section: "policy".into(),
                key: "max_notional_per_order".into(),
                value: "250".into(),
            },
            SettingRow { section: "flags".into(), key: "poly_exec".into(), value: "true".into() },
        ],
        ..Default::default()
    };
    let source = StoreLayer::Rows { rows: &rows, adopted: None };

    let d = describe_with_source(Some(dir.path()), source).unwrap();
    assert_eq!(
        d.settings,
        vike_config::load_with_source(
            Some(dir.path()),
            source,
            &vike_config::CliOverrides::default()
        )
        .unwrap()
    );
    assert_eq!(d.settings.policy.max_notional_per_order, Some(250.0));
}
