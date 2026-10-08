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

/// **The unread credential FILE is an ERROR line; every other finding a WARN; the disclosure INFO.**
/// Driven through the real findings over real directories, so the classifier keys on what the
/// producer actually prints.
#[test]
fn a_credential_file_with_no_database_is_the_one_error_line() {
    let unmigrated = tempfile::tempdir().unwrap();
    std::fs::write(unmigrated.path().join(vike_secrets::SECRETS_FILE), "BINANCE_DEMO_API_KEY=x\n")
        .unwrap();
    let findings = credential_store_findings(Some(unmigrated.path()));
    let unread: Vec<&String> =
        findings.iter().filter(|l| boot_line_level(l) == BootLineLevel::Error).collect();
    assert_eq!(unread.len(), 1, "exactly the unread-file line is an error: {findings:?}");
    assert!(unread[0].contains("NOT READ") && unread[0].contains("vike-cli secrets migrate"));

    // The disclosure line beside it, and a line that merely MENTIONS the phrase without the
    // finding's prefix, are not alarms.
    assert_eq!(
        boot_line_level(&credential_store_line(Some(unmigrated.path()))),
        BootLineLevel::Info
    );
    assert_eq!(
        boot_line_level("settings dir: /x — is on disk but is NOT READ"),
        BootLineLevel::Info
    );
    assert_eq!(boot_line_level("credential store: ⚠ anything else"), BootLineLevel::Warn);

    // An empty directory says nothing at error level.
    let empty = tempfile::tempdir().unwrap();
    assert!(
        credential_store_findings(Some(empty.path()))
            .iter()
            .all(|l| boot_line_level(l) != BootLineLevel::Error)
    );
}
