use super::*;

/// **The one address ladder, for both hubs**: the flag, then the setting, then the hub's compiled-in default.
/// A BLANK rung is an ABSENT one (a row or a flag set to `""` must not dial the empty string), and a padded
/// one is trimmed. Every door dials through these two, so none can aim a box at another hub than its
/// neighbour does.
#[test]
fn the_dial_address_is_the_flag_then_the_setting_then_the_default_and_a_blank_rung_is_absent() {
    for (dial, default) in [
        (
            datahub_dial_address as fn(Option<&str>, Option<&str>) -> String,
            vike_config::DEFAULT_DATAHUB_ADDRESS,
        ),
        (backtesthub_dial_address, vike_config::DEFAULT_BACKTEST_ADDRESS),
    ] {
        assert_eq!(dial(Some("<host>:1"), Some("<host>:2")), "<host>:1");
        assert_eq!(dial(None, Some("<host>:2")), "<host>:2");
        assert_eq!(dial(None, None), default);
        assert_eq!(dial(Some("   "), Some("<host>:2")), "<host>:2");
        assert_eq!(dial(Some(""), None), default);
        assert_eq!(dial(None, Some(" ")), default, "a blank SETTING is skipped too");
        assert_eq!(dial(Some(" <host>:1 "), None), "<host>:1");
    }
}

/// **The tradehub's ladder (variant A)**: the flag, then `config.node_addr`, blank rungs skipped and values trimmed as
/// above; with neither, the one refusal naming `vike-cli backend connect`, never a guessed port.
#[test]
fn the_node_is_the_flag_then_the_setting_and_neither_is_refused_naming_backend_connect() {
    use crate::refusals::{NodeAddressRefusal, NodeAddressRefusalId};
    assert_eq!(node_dial_address(Some("<host>:1"), Some("<host>:2")).unwrap(), "<host>:1");
    assert_eq!(node_dial_address(None, Some(" <host>:2 ")).unwrap(), "<host>:2");
    assert_eq!(node_dial_address(Some(" "), Some("<host>:2")).unwrap(), "<host>:2");
    for (flag, row) in [(None, None), (Some(""), Some("  "))] {
        let refusal = node_dial_address(flag, row).unwrap_err();
        assert_eq!(refusal, NodeAddressRefusal::Unset);
        assert_eq!(refusal.id(), NodeAddressRefusalId::Unset);
    }
    assert!(NodeAddressRefusal::Unset.rendered().starts_with("error [dial.node.unset]: "));
    assert!(NodeAddressRefusal::Unset.to_string().contains("`vike-cli backend connect`"));
}

/// A loader warning is SURFACED, never swallowed. [`Resolved::from_booted`] emits exactly what
/// [`settings_warning_lines`] returns, so proving a warning survives that step proves the
/// dispatcher prints it — the failure this guards is the loader resolving something the
/// operator did not write while they believe their own file is in force. (Warnings were
/// swallowed here until Phase 6c.)
///
/// ⚠ The warning is HAND-STUFFED, which it deliberately was not before. This drove
/// `Settings::clamp_to_policy` — `policy.rate.max_utilization` over
/// `preferences.rate_utilization` — and that clamp was removed with both of its fields: a
/// ceiling that bounded a value nothing read. The loader HAS a producer again
/// (`vike_config::NO_SETTINGS_DIRECTORY_WARNING`), and this test deliberately does not use it:
/// driving the real one would test the producer, where what this gates is the half that lives in
/// THIS crate and was the actual Phase-6c defect — a warning that exists reaches the operator,
/// verbatim.
#[test]
fn a_loader_warning_is_surfaced_not_swallowed() {
    let mut settings = vike_config::Settings::default();
    settings.warnings.push("preferences.something was resolved to 0.5".to_string());

    let lines = settings_warning_lines("a-program", &settings);
    assert_eq!(lines.len(), 1, "{lines:?}");
    assert!(lines[0].starts_with("a-program: settings: "), "the program's name: {}", lines[0]);
    assert!(lines[0].contains("preferences.something"), "names the key: {}", lines[0]);
    assert!(lines[0].contains("0.5"), "carried verbatim, not summarised: {}", lines[0]);
}

/// The quiet path stays quiet: nothing to resolve ⇒ nothing printed, so a line on stderr always
/// means something actually happened. Load-bearing for `vike-mcp`, whose stdout is a
/// protocol and whose stderr an agent may still read.
///
/// ⚠ The second half used to drive `load(None, …)` and assert silence, and that is no longer a
/// clean load — it is the "no project resolved" case, which now says so
/// (`vike_config::NO_SETTINGS_DIRECTORY_WARNING`). Both halves are kept and the second is
/// INVERTED rather than deleted: a `vike-cli` run from outside any project must still print
/// exactly one line, on stderr, and never on the stdout `mcp` speaks a protocol over.
#[test]
fn a_clean_load_prints_nothing_and_a_projectless_one_prints_exactly_one_line() {
    assert!(settings_warning_lines("a-program", &vike_config::Settings::default()).is_empty());
    let settings = vike_config::load(None).unwrap();
    let lines = settings_warning_lines("a-program", &settings);
    assert_eq!(lines.len(), 1, "no project ⇒ exactly one line: {lines:?}");
    assert!(lines[0].contains("no settings directory resolved"), "{}", lines[0]);
}

/// The one device-key read: no row is no key, a row is its key with the `person/device` it was registered as, and a
/// row whose document is no key is a fault carrying the row's key id (and no byte of it).
#[test]
fn a_device_key_row_reads_as_its_key_and_name_or_as_a_named_fault() {
    let (key, document) = vike_hub_protocol::access::DeviceKey::generate().expect("a key");
    let row = |private_key: Vec<u8>| vike_secrets::access::DeviceKeyRow {
        key_id: key.key_id(),
        person: "owner".to_string(),
        device: "pc".to_string(),
        private_key,
        created_ms: 0,
    };
    assert!(matches!(device_key_of(Ok(None)), Ok(None)));
    let Ok(Some((held, name))) = device_key_of(Ok(Some(row(document)))) else {
        panic!("a usable row");
    };
    assert_eq!((held.key_id(), name.as_str()), (key.key_id(), "owner/pc"));
    match device_key_of(Ok(Some(row(b"not a key".to_vec())))) {
        Err(DeviceKeyFault::NotAKey { key_id, .. }) => assert_eq!(key_id, key.key_id()),
        other => panic!("not a key reads {other:?}"),
    }
}

/// The refusal reads as the command the operator typed, and carries the mark's reason whole.
#[test]
fn the_seal_refusal_names_the_command_it_refuses() {
    let text = seal_refusal_text("a-program a-verb", "the seal is broken");
    assert!(text.starts_with("REFUSED: `a-program a-verb` will not run"), "{text}");
    assert!(text.contains("the seal is broken"), "{text}");
    assert!(text.ends_with(vike_config::UNSOUND_STORE_REPAIR), "{text}");
}
