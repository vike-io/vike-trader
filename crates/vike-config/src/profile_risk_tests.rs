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

/// ...and the shape half, in the DIRECTION the real parser actually refuses — see
/// [`ProfileRiskKey::accepts`], whose table is a measurement rather than a deduction.
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
/// wrong: an INTEGER for a float key. `vike_exec::ProfileRisk` accepts it (MEASURED, pinned in
/// `crates/vike-tradehub/tests/profile_risk_rows.rs`), so refusing it here would make
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
    assert_eq!(missing_keys(&found).len(), PROFILE_RISK_KEYS.len());
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
