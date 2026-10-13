//! **The settings database's rows, resolved through the REAL loader** — the integration twin of
//! `crates/vike-config/src/mirror.rs`'s own unit tests, driven through `load_with_source`/
//! `describe_with_source` rather than calling `apply_rows` directly.
//!
//! There is one source (`docs/decisions/0086`), so these are the properties that were never about
//! two sources — refusal by name, a bound still biting, the section vocabulary, the seal's
//! integrity checks — proven through hand-built [`StoredSettings`] fixtures.
//!
//! What is proven here:
//!
//! * [`a_key_only_in_the_store_is_resolved_from_it`] — the store layer is LIVE, end to end through
//!   `load_with_source` and `describe_with_source`, and `config show` attributes it to `db`.
//! * [`a_typod_row_key_is_refused_by_name_rather_than_read_as_nothing`] /
//!   [`a_wrong_typed_row_is_refused_by_name_rather_than_by_section_alone`] /
//!   [`a_bound_still_refuses_a_row`] / [`a_null_row_is_refused_rather_than_resolving_to_the_default`]
//!   — the four properties a settings FILE had that a bare table might not, all proven through the
//!   loader rather than through `apply_rows` in isolation.
//! * [`the_sections_this_crate_mirrors_are_the_sections_the_store_accepts`] — the section
//!   vocabulary is ONE list with two carriers, held equal — and
//!   [`policy_serializes_no_map_shaped_key`], so every policy key is one scalar row.
//! * The SEAL's integrity check, through the loader: row loss and row arrival
//!   ([`losing_rows_after_adoption_refuses_rather_than_resolving_to_the_defaults`]).
//! * [`an_unreadable_store_marks_the_resolution_rather_than_erring`] and
//!   [`assert_no_catastrophic_repair`] — a store that will not open marks rather than refuses, and no
//!   message in this design may suggest deleting `vike.db`.
//! * [`a_written_reconcile_refusal_names_config_set_and_no_file`] — the remedy class
//!   `crates/vike-config/src/remedy.rs` carries, proven through a real load: there is one rendering
//!   now, not two chosen by which source answered.

use std::path::Path;

use vike_config::{
    CliOverrides, Origin, Policy, StoreLayer, describe_with_source, load_with_source,
};
use vike_secrets::{Adoption, SETTINGS_SECTIONS, SettingRow, StoredSettings};

/// A store with NO seal — the ordinary state of a box nothing has ever written a row to.
fn unsealed(rows: &StoredSettings) -> StoreLayer<'_> {
    StoreLayer::Rows { rows, adopted: None }
}

/// A seal whose counts MATCH `rows`, as a real write would leave it.
///
/// ⚠ The counts are taken from the rows rather than typed, because that is exactly what the writer
/// does — `vike_secrets::write_setting_row_in` counts inside its own transaction — and a fixture
/// that typed them would be measuring a state the writer cannot produce. The tests that want a
/// MISMATCH build one deliberately and say so.
fn seal(rows: &StoredSettings) -> Adoption {
    Adoption {
        adopted_at: "2026-09-18T00:00:00Z".to_string(),
        tool_version: "test".to_string(),
        files_present: String::new(),
        setting_rows: rows.settings.len(),
    }
}

fn sealed<'a>(rows: &'a StoredSettings, seal: &'a Adoption) -> StoreLayer<'a> {
    StoreLayer::Rows { rows, adopted: Some(seal) }
}

/// **Load a sealed store and return the SEAL MARK it produced.**
///
/// `load_with_source` never returns `Err` for a seal/row problem — see
/// `crate::mirror::apply_rows`'s own doc for why: a refusal here would take `vike-cli config show`
/// and every `secrets` verb down with the box they exist to diagnose. So this helper asserts BOTH
/// halves: the load must not `Err`, and it must produce a mark.
fn seal_mark(dir: &Path, rows: &StoredSettings, seal: &Adoption, why: &str) -> String {
    let settings = load_with_source(Some(dir), sealed(rows, seal), &CliOverrides::default())
        .expect(
            "load_with_source must never return Err for a seal/row problem — see \
                 crate::mirror::apply_rows",
        );
    settings.seal_refusal.expect(why)
}

// ---------------------------------------------------------------------------------------------
// The store layer is live
// ---------------------------------------------------------------------------------------------

/// **A key only in the store resolves from it, end to end**, and `config show` attributes it to
/// `db` by name.
#[test]
fn a_key_only_in_the_store_is_resolved_from_it() {
    let dir = tempfile::tempdir().unwrap();
    let store = StoredSettings {
        settings: vec![SettingRow {
            section: "config".into(),
            key: "log_dir".into(),
            value: "\"/from/db\"".into(),
        }],
        ..Default::default()
    };

    let resolved =
        load_with_source(Some(dir.path()), unsealed(&store), &CliOverrides::default()).unwrap();
    assert_eq!(resolved.config.log_dir.as_deref(), Some(Path::new("/from/db")));

    let d = describe_with_source(Some(dir.path()), unsealed(&store)).unwrap();
    let row = d.rows.iter().find(|r| r.key == "config.log_dir").unwrap();
    assert_eq!(row.origin, Origin::Db);
    assert_eq!(row.origin.kind(), "db");
}

// ---------------------------------------------------------------------------------------------
// The four properties a file had, proven through the loader instead
// ---------------------------------------------------------------------------------------------

/// **Unknown-key refusal BY NAME**, through the loader. A typo'd ROW KEY is not a typo'd column, so
/// no `CHECK` can refuse it and the database refuses it in neither direction — only the read path,
/// materialising the row back through the patch type, can.
#[test]
fn a_typod_row_key_is_refused_by_name_rather_than_read_as_nothing() {
    let dir = tempfile::tempdir().unwrap();
    for (section, key) in [
        ("policy", "max_levrage"),
        ("config", "log_dri"),
        ("preferences", "log_levle"),
        ("flags", "reconcil"),
    ] {
        let store = StoredSettings {
            settings: vec![SettingRow {
                section: section.into(),
                key: key.into(),
                value: "1".into(),
            }],
            ..Default::default()
        };
        let resolved =
            load_with_source(Some(dir.path()), unsealed(&store), &CliOverrides::default()).expect(
                "a row problem MARKS rather than hard-erroring — see `crate::mirror::apply_rows`",
            );
        let refusal = resolved.seal_refusal.expect("the typo'd row must be marked illegal");
        assert!(refusal.contains(key), "the refusal must NAME {key}: {refusal}");
    }
}

/// **A WRONG-TYPED row is refused BY NAME too** — the half that has no help from serde: an `unknown
/// field` message carries the name inside it, while `invalid type: string "30000", expected u64`
/// carries no name at all. `vike_config::mirror`'s `offending_leaf` is what restores it.
///
/// The two innocent siblings are the load-bearing part: one sorts BEFORE the offending key and one
/// AFTER it in the object's own (`BTreeMap`) order, so an implementation that simply named the
/// first row, or the last, is red here.
#[test]
fn a_wrong_typed_row_is_refused_by_name_rather_than_by_section_alone() {
    let dir = tempfile::tempdir().unwrap();
    let store = StoredSettings {
        settings: vec![
            // sorts BEFORE the offender
            SettingRow {
                section: "policy".into(),
                key: "deadman_action".into(),
                value: "\"cancel_all\"".into(),
            },
            // the offender: `deadman_timeout_ms` is a `u64`, and this row is a STRING
            SettingRow {
                section: "policy".into(),
                key: "deadman_timeout_ms".into(),
                value: "\"30000\"".into(),
            },
            // ...and one that sorts AFTER it
            SettingRow {
                section: "policy".into(),
                key: "max_leverage".into(),
                value: "3.0".into(),
            },
        ],
        ..Default::default()
    };

    let msg = seal_mark(
        dir.path(),
        &store,
        &seal(&store),
        "a wrongly-typed row is ILLEGAL and must be marked",
    );
    assert!(msg.contains("deadman_timeout_ms"), "the refusal must NAME the offending row: {msg}");
    assert!(!msg.contains("deadman_action"), "...and not a row that is fine: {msg}");
    assert!(!msg.contains("max_leverage"), "...in either direction: {msg}");
    // The type check's own words survive alongside the name — the operator needs both.
    assert!(msg.contains("expected u64"), "{msg}");
}

/// **Validate-on-load survives**: a bound imported from `vike-model` still bites, and no `CHECK`
/// constraint restates it in SQL — which would be the split-brain the typed model exists to refuse.
#[test]
fn a_bound_still_refuses_a_row() {
    let dir = tempfile::tempdir().unwrap();
    let store = StoredSettings {
        settings: vec![SettingRow {
            section: "policy".into(),
            key: "market_slippage".into(),
            value: "0.9".into(),
        }],
        ..Default::default()
    };
    let msg = seal_mark(dir.path(), &store, &seal(&store), "an out-of-bound row must mark");
    assert!(msg.contains("market_slippage"), "{msg}");
}

/// **A `null` row is refused rather than resolving to the default.** JSON's one expressive
/// advantage over TOML is the one thing this column must not accept — `deny_unknown_fields` sees a
/// KNOWN field name and every patch field is an `Option`, so a hand-written `null` would resolve to
/// the default in silence. Driven through the loader, because "silently unset" is a state a daemon
/// boots in.
#[test]
fn a_null_row_is_refused_rather_than_resolving_to_the_default() {
    let dir = tempfile::tempdir().unwrap();
    let store = StoredSettings {
        settings: vec![SettingRow {
            section: "policy".into(),
            key: "max_notional_per_order".into(),
            value: "null".into(),
        }],
        ..Default::default()
    };
    let msg = seal_mark(dir.path(), &store, &seal(&store), "a null row must mark");
    assert!(msg.contains("max_notional_per_order"), "{msg}");
}

// ---------------------------------------------------------------------------------------------
// The section vocabulary is one list with two carriers, and every policy key is one scalar row
// ---------------------------------------------------------------------------------------------

/// The section vocabulary is ONE list with two carriers: this crate's [`vike_config::SETTING_SECTIONS`]
/// and the store's `SETTINGS_SECTIONS` (which is also the `setting.section` `CHECK`'s). They are
/// held equal rather than each being trusted, because a section this crate resolves and the store
/// refused would be a write that fails halfway on a live box.
#[test]
fn the_sections_this_crate_mirrors_are_the_sections_the_store_accepts() {
    let mut mine: Vec<&str> = vike_config::SETTING_SECTIONS.to_vec();
    let mut theirs: Vec<&str> = SETTINGS_SECTIONS.to_vec();
    mine.sort_unstable();
    theirs.sort_unstable();
    assert_eq!(mine, theirs);
}

/// **`Policy` serializes no map-shaped key**, so every policy key is exactly one scalar `setting`
/// row and `deny_unknown_fields` reaches all of them.
///
/// A map is the one shape `deny_unknown_fields` structurally cannot police: its KEYS are data, so a
/// typo'd venue or label inside one would be flattened into a dotted `setting` row that no roster
/// check ever sees. The per-venue and per-account arming maps were the last ones, and the account
/// table's own `tier`, `active` and `max_exposure` columns replaced them. Derived from a real
/// serialization rather than from a literal, so a map added to `Policy` fails here and has to be
/// given a row shape of its own instead.
#[test]
fn policy_serializes_no_map_shaped_key() {
    let table = toml::Table::try_from(Policy::default()).unwrap();
    let maps: Vec<&str> =
        table.iter().filter(|(_, v)| v.is_table()).map(|(k, _)| k.as_str()).collect();
    assert!(
        maps.is_empty(),
        "`Policy` serializes {maps:?} as a map. A map-shaped policy key must not be flattened into \
         dotted `setting` rows, where its KEY SPACE would have no roster check at all: give it a \
         row shape of its own (as the `account` table is for the per-account tier and exposure)."
    );
}

// ---------------------------------------------------------------------------------------------
// The SEAL: the probe, and the detector that makes it worth having
// ---------------------------------------------------------------------------------------------

/// **Row LOSS refuses (marks), naming what was lost.** The erase detector.
///
/// Once the rows are the ONLY settings layer, a `DELETE` against them is not *an operator chose the
/// defaults* — it is a ceiling or a flag that has quietly stopped being applied, on a
/// daemon that starts clean and reports every value as resolved.
///
/// ⚠ The counts are relative to what THIS store was last sealed on rather than to an absolute
/// expectation, which is what makes the check cost NO behaviour change on a box that never set a
/// ceiling.
#[test]
fn losing_rows_after_adoption_refuses_rather_than_resolving_to_the_defaults() {
    let dir = tempfile::tempdir().unwrap();
    let full = StoredSettings {
        settings: vec![SettingRow {
            section: "policy".into(),
            key: "max_notional_per_order".into(),
            value: "250".into(),
        }],
        ..Default::default()
    };
    let seal = seal(&full);

    // The control: with its rows intact the box resolves the ceiling it was sealed on.
    let ok =
        load_with_source(Some(dir.path()), sealed(&full, &seal), &CliOverrides::default()).unwrap();
    assert_eq!(ok.policy.max_notional_per_order, Some(250.0));

    // ...and with the row gone it MARKS rather than answering "no ceiling".
    let emptied = StoredSettings::default();
    let msg = seal_mark(
        dir.path(),
        &emptied,
        &seal,
        "a row that has silently stopped being applied is the failure this detects",
    );
    assert!(msg.contains("CHANGED"), "{msg}");
    assert_no_catastrophic_repair(&msg);

    // ⚠ ...and in the OTHER direction too. A row ARRIVING outside the writer is the same evidence:
    // somebody edited the store by hand, and that row is now a live ceiling nobody wrote.
    let extra = StoredSettings {
        settings: vec![
            full.settings[0].clone(),
            SettingRow {
                section: "policy".into(),
                key: "max_account_exposure".into(),
                value: "9000000".into(),
            },
        ],
        ..Default::default()
    };
    seal_mark(
        dir.path(),
        &extra,
        &seal,
        "a row that ARRIVED outside the writer is the same evidence and the same mark",
    );
}

// ---------------------------------------------------------------------------------------------
// A store that will not open, and the one repair this whole design permits
// ---------------------------------------------------------------------------------------------

/// **A store that cannot be READ AT ALL resolves to compiled-in defaults, and says so as DATA.**
///
/// The loader MARKS rather than refusing: `vike_config::load_with_source`'s own doc carries the
/// measurement — an unreadable store means an empty CREDENTIAL map and therefore an all-paper mount,
/// so a hard refusal here would take a daemon down without preventing anything. `vike-cli config
/// check` answers `Level::Fail`, which puts the stop at a deploy pre-flight instead.
#[test]
fn an_unreadable_store_marks_the_resolution_rather_than_erring() {
    let dir = tempfile::tempdir().unwrap();
    let resolved = load_with_source(
        Some(dir.path()),
        StoreLayer::Unreadable("disk I/O error"),
        &CliOverrides::default(),
    )
    .expect("the loader RESOLVES and marks; the ROOT decides whether that is fatal");

    let why = resolved
        .store_refusal
        .as_deref()
        .expect("a store that could not be read must be a SECOND channel a verb can gate on");
    assert!(why.contains("disk I/O error"), "{why}");
    assert!(
        resolved.warnings.iter().any(|w| w.contains("disk I/O error")),
        "…and ride the warnings channel too, so a root that only surfaces those still shows it"
    );
    assert_no_catastrophic_repair(why);
}

/// **No refusal in this design may teach an operator to delete the settings database.**
///
/// `vike_secrets::Backend` decides which store answers for a CREDENTIAL on one probe — the mere
/// existence of `settings/db/vike.db` — so deleting that file takes every venue on a migrated box
/// silently to paper AND destroys the only copy of its venue keys. It is the one repair that is
/// both plausible-looking and catastrophic, and it is reachable from this design rather than
/// introduced by it, which is why the rule is a gate and not a note. By ruling
/// (`docs/decisions/0086`), the ONLY repair is restoring `vike.db` from the box's nightly backup —
/// no command may claim otherwise.
///
/// ⚠ **The check is on TOKENS, and a substring version of it was written first and was WRONG.**
/// `msg.contains("rm ")` matched *"JSON has no **form** for"* in the stale-format hint — a false
/// positive on ordinary English, in a gate whose whole value is that it fires only on the real
/// thing. A gate that cries wolf on prose is a gate somebody deletes.
fn assert_no_catastrophic_repair(msg: &str) {
    let bad: Vec<&str> = msg
        .split(|c: char| c.is_whitespace() || c == '`')
        .filter(|t| {
            let t = t.trim_matches(|c: char| !c.is_alphanumeric() && c != '.' && c != '/');
            t.eq_ignore_ascii_case("rm") || t.ends_with("vike.db")
        })
        .collect();
    assert!(
        bad.is_empty(),
        "⚠ this message names {bad:?}. Deleting the settings database makes \
         `vike_secrets::Backend` answer `Files` for CREDENTIALS: every venue silently on paper, \
         and the only copy of the box's venue keys gone. No refusal in this design may suggest \
         it.\n{msg}"
    );
}

// ---------------------------------------------------------------------------------------------
// THE REMEDY FOLLOWS THE ONE STORE THERE IS (`vike_config::remedy`)
// ---------------------------------------------------------------------------------------------

/// **The written-refusal warning names `config set` and no file, through a real load.**
///
/// `reconcile = false` arrives as a ROW — there is no settings file to have arrived in instead — so
/// the way out has to be the verb that writes a row, and this proves the real loader renders it,
/// not merely that `crate::flags::reconcile_refusal_ignored` does in isolation.
///
/// ⚠ There is one store, so there is one rendering — and the assertion must be one a file-naming
/// rendering would FAIL: an assertion satisfied under both would prove nothing.
#[test]
fn a_written_reconcile_refusal_names_config_set_and_no_file() {
    let dir = tempfile::tempdir().unwrap();
    let store = StoredSettings {
        settings: vec![SettingRow {
            section: "flags".into(),
            key: "reconcile".into(),
            value: "false".into(),
        }],
        ..Default::default()
    };

    let resolved = load_with_source(Some(dir.path()), unsealed(&store), &CliOverrides::default())
        .expect("a load with one legal row must not Err");

    let refusal = resolved
        .warnings
        .iter()
        .find(|m| m.contains("no longer turns reconciliation OFF"))
        .unwrap_or_else(|| panic!("the written refusal must still warn: {:?}", resolved.warnings));

    assert!(
        refusal.contains("`vike-cli config set flags.reconcile_off true`"),
        "the way out must be a command that reaches the ROWS: {refusal}"
    );
    assert!(
        !refusal.contains("flags.toml"),
        "⚠ THE DEFECT this class fixed: naming a settings file as somewhere to write sends the \
         operator to write it, restart, and get this identical warning back: {refusal}"
    );
    // …and the SUBSTANCE is untouched: the point of the line is that a written `false` no longer
    // means what it says.
    assert!(refusal.contains("FORCE-ON"), "{refusal}");
    assert!(!refusal.contains("VIKE_RECONCILE_OFF"), "no retired variable is a way out: {refusal}");
    assert_no_catastrophic_repair(refusal);
}
