use super::*;

/// A record named `name` on the the CI box example address, unarmed — also the editor tests' fixture
/// (`crate::backend::editor`'s tests).
pub(crate) fn rec(name: &str) -> BackendRecord {
    BackendRecord {
        name: name.to_string(),
        address: "the CI box.example:9040".to_string(),
        observe_key: String::new(),
        control_key: None,
        control: false,
    }
}

/// A populated registry survives a disk round-trip byte-for-value: records, order, the
/// active pointer, and every field including the arming gate.
#[test]
fn round_trips_through_disk() {
    let tmp = tempfile::tempdir().expect("temp dir");
    let p = tmp.path().join(BACKENDS_FILE);
    let mut armed = rec("the build runner");
    armed.control = true;
    let file = BackendRegistry {
        backends: vec![rec("the build runner"), armed],
        active: Some("the build runner".to_string()),
        refused: None,
    };
    save_to(&file, &p).expect("save");
    let back = load_from(&p).expect("load");
    assert_eq!(back, file);
}

/// An unknown key, at the top level or inside a record, refuses the whole file: nothing in it is
/// read, the hint names the file, and a save onto it is refused.
#[test]
fn a_file_with_an_unknown_key_is_refused() {
    let tmp = tempfile::tempdir().expect("temp dir");
    let p = tmp.path().join(BACKENDS_FILE);
    for raw in [
        r#"{"backends": [{"name": "the CI box", "address": "h:1", "from_the_future": true}]}"#,
        r#"{"backends": [], "active": null, "schema_note": "a key this build never heard of"}"#,
    ] {
        std::fs::write(&p, raw).expect("write");
        let hint = load_from(&p).expect_err("an unknown key must refuse the file");
        assert!(hint.contains(&p.display().to_string()), "the hint names the file: {hint}");
        assert!(hint.contains("nothing is saved over it"), "{hint}");
        assert!(save_to(&BackendRegistry::default(), &p).is_err(), "a save must not write over it");
        assert_eq!(std::fs::read_to_string(&p).expect("read back"), raw, "the file is untouched");
    }
}

/// THE OWNER'S RULING OF 2026-11-17: a `backends.json` from an older build (it says `address`) is
/// REFUSED with a one-line hint naming the file and the remedy; nothing in it is read (no record
/// with a blank address), the bytes on disk stay as they were, and a save onto it is refused.
#[test]
fn an_old_backends_file_naming_addr_is_refused_and_left_as_it_is() {
    let tmp = tempfile::tempdir().expect("temp dir");
    let p = tmp.path().join(BACKENDS_FILE);
    let old = r#"{"backends":[{"name":"the CI box","addr":"127.0.0.1:7879","observe_key":"K","control_key":null,"control":false,"datahub_observe_key":""}],"active":"the CI box"}"#;
    std::fs::write(&p, old).expect("write the old file");

    let hint = load_from(&p).expect_err("an old file must be refused");
    assert!(hint.contains(&p.display().to_string()), "the hint names the file: {hint}");
    assert!(hint.contains("older Vike Trader"), "{hint}");
    assert!(hint.contains("Data Manager → Backend"), "the hint says what to do: {hint}");

    let loaded = load_or_refused_from(&p);
    assert!(loaded.backends.is_empty() && loaded.active.is_none(), "nothing was read: {loaded:?}");
    assert_eq!(loaded.refused.as_deref(), Some(hint.as_str()));

    let file = BackendRegistry { backends: vec![rec("new")], ..BackendRegistry::default() };
    let err = save_to(&file, &p).expect_err("a save must not write over a refused file");
    assert!(err.to_string().contains("older Vike Trader"), "{err}");
    assert_eq!(std::fs::read_to_string(&p).expect("read back"), old, "the old file is untouched");
}

/// Missing fields fill with defaults: no `control` key loads as UNARMED, no `control_key` as
/// `None`, no `active` as `None`. The default being the DISARMED state is load-bearing: a file can
/// never arm a write channel by omission.
#[test]
fn a_file_with_missing_fields_loads_disarmed() {
    let raw = r#"{"backends": [{"name": "a", "address": "h:1", "observe_key": "K"}]}"#;
    let f: BackendRegistry = serde_json::from_str(raw).expect("a partial file must parse");
    assert_eq!(f.backends.len(), 1);
    assert!(!f.backends[0].control, "absent `control` must default to DISARMED");
    assert_eq!(f.backends[0].control_key, None);
    assert_eq!(f.active, None);
}

/// An absent file is the ordinary "no backends yet" state — the empty default, not an
/// error. A present-but-corrupt file answers an EMPTY registry holding the hint (never brick
/// startup, never read it silently).
#[test]
fn missing_file_is_an_empty_default() {
    let tmp = tempfile::tempdir().expect("temp dir");
    let absent = tmp.path().join(BACKENDS_FILE);
    assert_eq!(load_or_refused_from(&absent), BackendRegistry::default());

    let corrupt = tmp.path().join("corrupt.json");
    std::fs::write(&corrupt, "{ not json").expect("write corrupt");
    let refused = load_or_refused_from(&corrupt);
    assert!(refused.backends.is_empty() && refused.active.is_none());
    assert!(refused.refused.is_some_and(|hint| hint.contains("could not be read")));
}

/// ⚠ **A desktop without a device key sends nothing** (decision 0288 §3): a read and a write both
/// refuse with [`no_device_key`] for the record's own address, never a shared key, and a write to an
/// UNARMED record is refused before the key is asked (B9). This test binary never holds a device key:
/// [`fill_hub_keys`] is its one writer, and no unit test calls it.
#[test]
fn no_signer_without_a_device_key_and_no_write_signer_for_an_unarmed_record() {
    let refusal =
        |r: Result<DeviceKey, HubCallError>| r.err().map_or(String::new(), |e| e.to_string());
    let mut armed = rec("the CI box");
    armed.control = true;
    assert_eq!(refusal(observe_signer(&armed.address)), no_device_key(&armed.address));
    assert_eq!(refusal(control_signer(&armed)), no_device_key(&armed.address));
    // …and the RECORD's address picks the cure: a loopback record gets this box's, which differs from
    // the remote one above, so an address dropped or swapped on the way fails here.
    let mut local = armed.clone();
    local.address = "127.0.0.1:9301".to_string();
    assert_ne!(no_device_key(&local.address), no_device_key(&armed.address));
    assert_eq!(refusal(observe_signer(&local.address)), no_device_key(&local.address));
    assert_eq!(refusal(control_signer(&local)), no_device_key(&local.address));
    assert!(refusal(control_signer(&rec("the CI box"))).contains("not armed"), "B9");
    // Both refusals dialled nothing, and say so to whoever reads the outcome (a settings write's fold).
    for refused in [control_signer(&rec("the CI box")), control_signer(&armed)] {
        let why = refused.err();
        assert!(matches!(why, Some(HubCallError::NotSent(_))), "{why:?}");
    }
}

/// A hostile name full of path separators is neutralized on save: what lands on disk
/// carries no separator, the `active` pointer is sanitized with the same function so it
/// still matches, and the registry directory holds exactly the one file — nothing escaped,
/// and no `.tmp` residue survives the write.
#[test]
fn a_hostile_name_cannot_escape_the_directory() {
    let tmp = tempfile::tempdir().expect("temp dir");
    let p = tmp.path().join(BACKENDS_FILE);
    let hostile = "../../evil\\..\\name";
    let file = BackendRegistry {
        backends: vec![rec(hostile)],
        active: Some(hostile.to_string()),
        refused: None,
    };
    save_to(&file, &p).expect("save");

    let back = load_from(&p).expect("load");
    let name = &back.backends[0].name;
    assert!(
        !name.contains('/') && !name.contains('\\') && !name.contains(".."),
        "sanitized name still carries a path component: {name:?}"
    );
    assert_eq!(
        back.active.as_deref(),
        Some(name.as_str()),
        "the active pointer must be sanitized by the same function, so it still matches"
    );
    let entries: Vec<_> = std::fs::read_dir(tmp.path())
        .expect("read dir")
        .map(|e| e.expect("entry").file_name().to_string_lossy().into_owned())
        .collect();
    assert_eq!(entries, vec![BACKENDS_FILE.to_string()], "exactly one file, no escapees");
}
// The device key's STORE-backed tests (a stored key is what every dial signs with, an unreadable
// store is named) live in `crates/vike-app-core/tests/hub_key_store_spellings.rs`: they plant a
// `device_key` row, and a `src` test file that calls a credential writer is a row of `WRITER_CALLERS`
// (`crates/vike-ops/tests/settings_secrets/credential_writer_gate.rs`) this change has no reason to add.
