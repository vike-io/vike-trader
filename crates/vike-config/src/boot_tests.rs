use super::*;
use crate::provenance::Origin;

fn row(key: &str, value: Option<&str>, origin: Origin) -> ResolvedSetting {
    ResolvedSetting {
        key: key.to_string(),
        section: "policy",
        value: value.map(str::to_string),
        default: None,
        origin,
        origin_value: value.map(str::to_string),
        adjusted: false,
    }
}

/// A credential-shaped KEY never renders its value, whichever layer set it — the insurance the
/// module doc argues for, exercised with a field the model does not have yet.
#[test]
fn a_credential_shaped_key_renders_set_not_its_value() {
    let secret = row("config.bot_token", Some("hunter2"), Origin::Db);
    let line = setting_line(&secret);
    assert!(!line.contains("hunter2"), "the value leaked: {line}");
    assert!(line.contains(SET), "{line}");
    // …and the ORIGIN is still reported: hiding a value must not hide which layer set it.
    assert!(line.contains("db"), "{line}");
}

/// An unset secret is `<unset>`, not `<set>`: a credential's compile-time default is always
/// "absent" (absent credentials ARE the live gate), and an empty string is the same answer.
#[test]
fn an_absent_secret_is_unset_not_set() {
    for value in [None, Some("")] {
        let line = setting_line(&row("config.api_key", value, Origin::Default));
        assert!(line.contains(UNSET), "{line}");
        assert!(!line.contains(SET), "{line}");
    }
}

/// An ordinary knob is printed verbatim — the redaction must not swallow the disclosure.
#[test]
fn an_ordinary_setting_prints_its_value_and_origin() {
    let line = setting_line(&row("policy.max_leverage", Some("3"), Origin::Db));
    assert_eq!(line, "setting: policy.max_leverage = 3 [db]");
}

/// The store arm of a test that plants no settings database.
const NOT_CONSULTED: StoreLayer<'static> =
    StoreLayer::NotConsulted("boot_tests: no settings database");

/// **A `credential store: ⚠` finding is a WARN line; the disclosure is INFO**, each set by its
/// producer and never read back from the words: a directory NAMED like a finding is still the INFO
/// disclosure line that names it.
#[test]
fn a_store_finding_is_a_warn_line_and_the_disclosure_is_info() {
    let parent = tempfile::tempdir().unwrap();
    let dir = parent.path().join("credential store ⚠ quoted");
    std::fs::create_dir_all(&dir).unwrap();
    let lines = boot_lines(Some(&dir), NOT_CONSULTED);
    assert!(lines.iter().all(|l| l.level == BootLineLevel::Info), "{lines:#?}");
    assert!(lines.iter().any(|l| l.text.contains("credential store ⚠ quoted")), "{lines:#?}");
    // An empty directory has no store, so there is no store finding at all.
    assert!(credential_store_findings(Some(&dir)).is_empty());
}

/// Every `setting: <key> = …` line carries its key, and no other line carries one: a root that reads only
/// some settings leaves the rest out by the key, never by the line's words.
#[test]
fn a_setting_line_names_its_key_and_no_other_line_does() {
    let lines = boot_lines(None, NOT_CONSULTED);
    for line in &lines {
        match &line.setting {
            Some(key) => assert!(line.text.starts_with(&format!("setting: {key} = ")), "{line:?}"),
            None => assert!(!line.text.starts_with("setting: "), "{line:?}"),
        }
    }
    assert!(lines.iter().any(|l| l.setting.as_deref() == Some("policy.max_notional_per_order")));
}
