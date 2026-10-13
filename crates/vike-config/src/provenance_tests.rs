use super::*;
use std::assert_matches;

#[test]
fn the_origin_words_are_pinned() {
    assert_eq!(Origin::Default.kind(), "default");
    assert_eq!(Origin::Db.kind(), "db");

    assert_eq!(Origin::Db.label(), "db");
    assert_eq!(Origin::Default.detail(), "");
    assert_eq!(Origin::Db.detail(), "db/vike.db");

    // TWO words, and the three retired ones (`project`, `file`, `env`) must never come back as a
    // spelling `--json` emits: the layers they named are refused at load or deleted outright.
    let words: Vec<&str> = [Origin::Default, Origin::Db].iter().map(Origin::kind).collect();
    assert_eq!(words, ["default", "db"]);
}

/// `1` and `1.0` are the same ceiling; `info` and `debug` are not.
#[test]
fn the_same_value_test_is_numeric_when_it_can_be() {
    assert!(same_value(Some("1"), Some("1.0")));
    assert!(same_value(Some("0.50"), Some("0.5")));
    assert!(same_value(Some("info"), Some("info")));
    assert!(!same_value(Some("0.9"), Some("0.5")));
    assert!(!same_value(Some("info"), Some("debug")));
    assert!(!same_value(Some("x"), None));
    assert!(same_value(None, None));
}

/// An `Option::None` field must SERIALIZE AWAY rather than error — the whole value side rests
/// on it, and `toml` has no null to represent it with.
#[test]
fn an_unset_optional_field_is_absent_from_the_serialized_section() {
    let sections = sections(&Settings::default());
    let policy = &sections["policy"];
    assert!(policy.contains_key("max_leverage"), "a set field is present: {policy:?}");
    assert!(
        !policy.contains_key("max_notional_per_order"),
        "an unset Option must be absent, not an error: {policy:?}"
    );
    assert_eq!(lookup(Some(policy), &["max_notional_per_order"]), None);
}

/// The derived half must cover the registry exactly — one row per flag, keyed and pathed by the
/// field name.
#[test]
fn every_registered_flag_has_a_row() {
    let keys = setting_keys();
    for meta in FLAG_REGISTRY {
        let want = format!("flags.{}", meta.field);
        let row = keys.iter().find(|k| k.key == want).unwrap_or_else(|| panic!("{want}"));
        assert_eq!(row.path, vec![meta.field]);
    }
    assert_eq!(keys.iter().filter(|k| k.section == "flags").count(), FLAG_REGISTRY.len());
}

/// No row names a retired arming key: an account's tier and exposure figure are rows of the
/// `account` table (decision 0119), not settings, so `config show` discloses no `policy.venues.*`,
/// `policy.accounts` or `policy.account_exposure` row.
#[test]
fn no_row_names_a_retired_arming_key() {
    for k in setting_keys() {
        for retired in ["policy.venues", "policy.accounts", "policy.account_exposure"] {
            assert!(
                k.key != retired && !k.key.starts_with(&format!("{retired}.")),
                "{} is a retired arming key",
                k.key
            );
        }
    }
}

#[test]
fn keys_are_sorted_and_prefixed_by_their_section() {
    let keys = setting_keys();
    let names: Vec<&str> = keys.iter().map(|k| k.key.as_str()).collect();
    let mut sorted = names.clone();
    sorted.sort_unstable();
    assert_eq!(names, sorted);
    for k in &keys {
        let section = k.key.split('.').next().unwrap();
        assert_matches!(
            section,
            "policy" | "config" | "preferences" | "flags",
            "{} has no known section",
            k.key
        );
    }
}

#[test]
fn the_precedence_line_names_db_then_default() {
    let line = precedence_line();
    assert!(line.starts_with("precedence: the settings database > default"), "{line}");
    assert!(line.contains("STORE ONLY"), "{line}");
}
