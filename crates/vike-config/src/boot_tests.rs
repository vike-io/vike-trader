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

/// **A `credential store: ⚠` finding is a WARN line; the disclosure is INFO.** Driven through the
/// real store line, so the classifier keys on what the producer actually prints.
#[test]
fn a_store_finding_is_a_warn_line_and_the_disclosure_is_info() {
    let dir = tempfile::tempdir().unwrap();
    assert_eq!(boot_line_level(&credential_store_line(Some(dir.path()))), BootLineLevel::Info);
    assert_eq!(boot_line_level("credential store: ⚠ anything"), BootLineLevel::Warn);
    // A line that merely MENTIONS the prefix's words without starting with it is not a finding.
    assert_eq!(
        boot_line_level("settings dir: /x — credential store: ⚠ quoted"),
        BootLineLevel::Info
    );
    // An empty directory has no store, so there is no store finding at all.
    assert!(credential_store_findings(Some(dir.path())).is_empty());
}
