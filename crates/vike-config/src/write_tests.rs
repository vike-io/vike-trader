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
/// collateral-promotion guard, enforced by `differing_keys`.
#[test]
fn a_bad_key_refuses_before_any_database_is_touched() {
    let d = dir();
    let err =
        write_setting_row(d.path(), "confgi.api_key", "x", std::time::Duration::from_millis(500))
            .expect_err("an unknown section must refuse");
    assert!(matches!(err, RowPlanError::BadKey { .. }), "{err:?}");
    assert!(!d.path().join("vike.db").exists(), "nothing was created for a refusal");
}

/// A store holding one arming row, so a candidate resolves and an ordinary write can land.
fn seeded() -> tempfile::TempDir {
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
    d
}

/// **Every credential shape is refused by the PLANNER, before the database is opened** — both arms
/// of `crate::redact::is_secret`: a leaf that ENDS in a shape (`config.acme_token`) and a leaf that
/// IS the bare shape (`preferences.token`). The keys are built from
/// `crate::redact::SECRET_SUFFIXES` itself, so a shape added there is covered here unasked.
///
/// "Before the database is opened" is proven two ways: a SEEDED store is byte-identical afterwards,
/// and a directory with NO database answers the credential refusal rather than `NoDatabase`, which
/// it could only do if the refusal ran first.
#[test]
fn every_credential_shape_is_refused_before_the_database_is_opened() {
    let budget = std::time::Duration::from_millis(500);
    let value = "a-value-nobody-may-store";
    let d = seeded();
    let db = vike_secrets::db_path_in(d.path());
    let before = std::fs::read(&db).expect("the database");
    let empty = dir();

    for suffix in crate::redact::SECRET_SUFFIXES {
        let lower = suffix.to_ascii_lowercase();
        for key in [format!("config.acme{lower}"), format!("preferences.{}", &lower[1..])] {
            assert!(
                matches!(refuse_credential_key(&key), Err(RowPlanError::CredentialKey { .. })),
                "{key}"
            );
            let err = write_setting_row(d.path(), &key, value, budget)
                .expect_err("a credential-shaped key must be refused");
            assert!(matches!(err, RowPlanError::CredentialKey { .. }), "{key}: {err:?}");
            let msg = err.to_string();
            assert!(msg.contains(&key), "it names the key: {msg}");
            assert!(!msg.contains(value), "it never repeats the value: {msg}");
            assert!(msg.contains("secrets set") && msg.contains("stdin"), "{msg}");
            assert!(
                msg.contains("config show"),
                "a mistyped SETTING is pointed at the list: {msg}"
            );

            let err = write_setting_row(empty.path(), &key, value, budget)
                .expect_err("no database, and a credential-shaped key");
            assert!(
                matches!(err, RowPlanError::CredentialKey { .. }),
                "the refusal must run before the database is looked for: {key}: {err:?}"
            );
        }
    }
    assert_eq!(std::fs::read(&db).expect("the database"), before, "byte-identical");
    assert!(!vike_secrets::db_path_in(empty.path()).exists(), "nothing was created");

    // The anti-vacuity control: the same store takes an ordinary key, so the refusals above are
    // the fence and not a store that refuses everything.
    write_setting_row(d.path(), "config.tradehub_addr", "127.0.0.1:7879", budget)
        .expect("an ordinary settings key lands");
    refuse_credential_key("policy.max_notional_per_order").expect("an ordinary key passes");
}

/// **An ARMING key is not judged by its label.** `KEY` and `USER` are legal account labels, and an
/// arming row's value is a mode word, so `policy.accounts.<venue>.<LABEL>` must pass the fence even
/// though its leaf matches a credential shape. The exemption is the ROW SHAPE, not the word: the
/// same leaf on a plain settings row is still refused.
#[test]
fn an_arming_key_is_not_judged_by_its_label() {
    for key in ["policy.accounts.binance.KEY", "policy.accounts.binance.USER"] {
        assert!(
            crate::redact::is_secret_key(key),
            "precondition: {key}'s leaf is credential-shaped"
        );
        refuse_credential_key(key).expect("an account label is not a credential name");
    }
    let d = seeded();
    let landed = write_setting_row(
        d.path(),
        "policy.accounts.binance.KEY",
        "paper",
        std::time::Duration::from_millis(500),
    );
    assert!(
        !matches!(landed, Err(RowPlanError::CredentialKey { .. })),
        "an arming write must not meet the credential fence: {landed:?}"
    );
    for key in ["config.key", "flags.user"] {
        assert!(
            matches!(refuse_credential_key(key), Err(RowPlanError::CredentialKey { .. })),
            "{key}"
        );
    }
}
