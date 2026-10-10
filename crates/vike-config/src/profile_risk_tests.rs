use super::*;

fn profile_with(body: &str) -> (tempfile::TempDir, std::path::PathBuf) {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("run-live.toml");
    std::fs::write(&path, body).unwrap();
    (tmp, path)
}

#[test]
fn a_risk_table_becomes_one_row_per_key_carrying_its_toml_rendering() {
    let (_tmp, path) = profile_with(
        "name = \"x\"\nmode = \"live\"\n\n[risk]\nmax_notional_per_order = 250.0\n\
             max_total_exposure = 1000.0\nmax_orders_per_window = 20\n\
             block_reduce_only_overshoot = true\n",
    );
    let found = risk_rows_from_profile(&path).unwrap();
    assert_eq!(found.profile, "run-live.toml");
    assert_eq!(
        found.rows,
        vec![
            ProfileRiskRow { key: "block_reduce_only_overshoot".into(), value: "true".into() },
            ProfileRiskRow { key: "max_notional_per_order".into(), value: "250.0".into() },
            ProfileRiskRow { key: "max_orders_per_window".into(), value: "20".into() },
            ProfileRiskRow { key: "max_total_exposure".into(), value: "1000.0".into() },
        ]
    );
}

/// The roster's read half: an unknown key is refused BY NAME, exactly as
/// `deny_unknown_fields` refuses it when the boot reads the same file.
#[test]
fn an_unknown_risk_key_is_refused_by_name() {
    let (_tmp, path) = profile_with("[risk]\nmax_levrage = 3.0\n");
    let err = risk_rows_from_profile(&path).unwrap_err();
    let msg = err.to_string();
    assert!(msg.contains("max_levrage"), "{msg}");
    assert!(msg.contains("ProfileRisk"), "the refusal must name the type that agrees: {msg}");
}

/// ...and the shape half, in the DIRECTION the real parser actually refuses: the value is judged by
/// `vike_model::ProfileRisk`'s own deserialize, and the refusal names the shape the key has.
#[test]
fn a_value_of_the_wrong_scalar_shape_is_refused_by_name() {
    let (_tmp, path) = profile_with("[risk]\nmax_orders_per_window = 20.0\n");
    let err = risk_rows_from_profile(&path).unwrap_err().to_string();
    assert!(err.contains("max_orders_per_window"), "{err}");
    assert!(err.contains("integer"), "{err}");

    let (_tmp, path) = profile_with("[risk]\nblock_reduce_only_overshoot = 1\n");
    let err = risk_rows_from_profile(&path).unwrap_err().to_string();
    assert!(err.contains("boolean"), "{err}");
}

/// **The direction the mirror must NOT refuse**, and the one a first attempt at this module got
/// wrong: an INTEGER for a float key. `vike_model::ProfileRisk` accepts it (MEASURED, pinned in
/// `crates/vike-config/tests/profile_risk_rows.rs`), so refusing it here would make
/// `config mirror --profile` reject a profile the daemon starts on — a mirror stricter than the
/// thing it mirrors, teaching a false rule about the file that judges orders.
#[test]
fn an_integer_is_accepted_for_a_float_key_because_the_real_parser_accepts_one() {
    let (_tmp, path) = profile_with("[risk]\nmax_leverage = 3\n");
    let found = risk_rows_from_profile(&path).expect("the daemon starts on this file");
    assert_eq!(found.rows, vec![ProfileRiskRow { key: "max_leverage".into(), value: "3".into() }]);
}

#[test]
fn a_profile_with_no_risk_table_mirrors_no_rows_rather_than_failing() {
    let (_tmp, path) = profile_with("name = \"x\"\nmode = \"paper\"\n");
    let found = risk_rows_from_profile(&path).unwrap();
    assert!(found.rows.is_empty());
    assert_eq!(missing_keys(&found), vike_model::ProfileRisk::keys());
}

#[test]
fn a_risk_key_that_is_not_a_scalar_is_refused_rather_than_flattened() {
    let (_tmp, path) = profile_with("[risk]\nmax_leverage = [1.0, 2.0]\n");
    assert!(risk_rows_from_profile(&path).unwrap_err().to_string().contains("max_leverage"));
    let (_tmp, path) = profile_with("risk = 3\n");
    assert!(risk_rows_from_profile(&path).unwrap_err().to_string().contains("TABLE"));
}

#[test]
fn an_absent_profile_file_is_an_error_naming_it() {
    let tmp = tempfile::tempdir().unwrap();
    let err = risk_rows_from_profile(&tmp.path().join("nope.toml")).unwrap_err().to_string();
    assert!(err.contains("nope.toml"), "{err}");
}

/// The store-side defect the roster's read half exists for: a row nothing in this tree can
/// write, named rather than rendered as a ceiling.
#[test]
fn a_row_whose_key_is_not_in_the_roster_is_reported_as_unknown() {
    let stored = StoredProfileRisk {
        profile: "run-live.toml".into(),
        rows: vec![
            ProfileRiskRow { key: "max_leverage".into(), value: "3.0".into() },
            ProfileRiskRow { key: "max_levrage".into(), value: "9.0".into() },
        ],
    };
    let unknown = unknown_rows(&stored);
    assert_eq!(unknown.len(), 1);
    assert_eq!(unknown[0].key, "max_levrage");
}

/// The JOIN onto the ceilings table, rather than a second copy of its flag.
#[test]
fn the_two_mount_refusing_keys_join_onto_their_ceiling_rows() {
    for name in ["max_notional_per_order", "max_total_exposure"] {
        let c = ceiling_for(name).unwrap_or_else(|| panic!("{name} has no run-profile ceiling"));
        assert!(c.refuses_live_mount_when_absent, "{name}");
    }
    assert!(ceiling_for("tick_size").is_none(), "a venue-owned grid field is not a ceiling row");
}

#[test]
fn a_profile_is_named_by_its_file_name_and_a_nameless_path_keeps_its_whole_spelling() {
    assert_eq!(profile_name_of(Path::new("/srv/x/settings/run-live.toml")), "run-live.toml");
    assert_eq!(profile_name_of(Path::new("run-live.toml")), "run-live.toml");
    assert!(!profile_name_of(Path::new("..")).is_empty());
}

/// **The `BOUNDS` prose is held to the TYPE, not to a second list**: one row per key of
/// `vike_model::ProfileRisk::keys()` (serde's own field list), in the struct's order, each one
/// legible. A new `[risk]` field reddens here until it has a sentence; a removed one reddens until
/// its sentence goes. The column is `vike-cli config show`'s, and the owner kept it (decision 0114).
#[test]
fn the_bounds_table_is_the_types_own_key_list_in_order() {
    let names: Vec<&str> = RISK_KEY_BOUNDS.iter().map(|(name, _)| *name).collect();
    assert_eq!(names, vike_model::ProfileRisk::keys(), "RISK_KEY_BOUNDS must follow the type");
    for (name, what) in RISK_KEY_BOUNDS {
        assert!(what.len() > 20, "`{name}`'s bounds sentence is too short to say what it bounds");
        assert_eq!(risk_key_bounds(name), Some(*what));
    }
}

/// **Every key's scalar shape is worked out by the REAL parser**, and a shape it cannot classify is
/// a STOP, not a default: a future `[risk]` field of a new kind (a string, a table) needs a
/// `RiskKeyKind` and a decision about what a row may hold. The four pins below are the answers the
/// mirror's refusals are written against.
#[test]
fn every_key_classifies_through_the_real_parser() {
    for key in vike_model::ProfileRisk::keys() {
        assert!(
            risk_key_kind(key).is_some(),
            "`ProfileRisk::{key}` has a shape no probe classifies — extend `RiskKeyKind` and \
             `risk_key_kind` deliberately, never default it"
        );
    }
    assert_eq!(risk_key_kind("max_leverage"), Some(RiskKeyKind::Float));
    assert_eq!(risk_key_kind("max_orders_per_window"), Some(RiskKeyKind::Integer));
    assert_eq!(risk_key_kind("window_ms"), Some(RiskKeyKind::Integer));
    assert_eq!(risk_key_kind("block_reduce_only_overshoot"), Some(RiskKeyKind::Boolean));
}

/// A name outside the type has neither a shape nor a sentence, so `config show` renders it as the
/// unknown row it is rather than as a ceiling.
#[test]
fn a_name_outside_the_type_has_no_kind_and_no_bounds() {
    assert_eq!(risk_key_kind("max_levrage"), None);
    assert_eq!(risk_key_bounds("max_levrage"), None);
}

/// The JOIN onto `PRE_TRADE_CEILINGS`, typed: every run-profile ceiling row must be a key of the
/// real type, or `config show` would name a ceiling the mirror can never carry a value for.
#[test]
fn every_run_profile_ceiling_is_a_key_of_the_type() {
    for c in crate::ceilings::PRE_TRADE_CEILINGS {
        if c.home != crate::ceilings::CeilingHome::RunProfileRisk {
            continue;
        }
        assert!(
            vike_model::ProfileRisk::keys().contains(&c.name),
            "`PRE_TRADE_CEILINGS` carries a run-profile ceiling `{}` that is no `ProfileRisk` key",
            c.name
        );
    }
}
