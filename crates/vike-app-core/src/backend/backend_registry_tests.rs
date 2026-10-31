use super::*;

fn rec(name: &str) -> BackendRecord {
    BackendRecord {
        name: name.to_string(),
        addr: "the CI box.example:9040".to_string(),
        observe_key: "PROD2_OBSERVE_KEY".to_string(),
        control_key: Some("PROD2_CONTROL_KEY".to_string()),
        control: false,
        datahub_observe_key: String::new(),
    }
}

fn vars(pairs: &[(&str, &str)]) -> HashMap<String, String> {
    pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect()
}

/// A populated registry survives a disk round-trip byte-for-value: records, order, the
/// active pointer, and every field including the arming gate.
#[test]
fn round_trips_through_disk() {
    let tmp = tempfile::tempdir().expect("temp dir");
    let p = tmp.path().join(BACKENDS_FILE);
    let mut armed = rec("the build runner");
    armed.control = true;
    let file = BackendsFile {
        backends: vec![rec("the build runner"), armed],
        active: Some("the build runner".to_string()),
    };
    save_to(&file, &p).expect("save");
    let back = load_from(&p).expect("load");
    assert_eq!(back, file);
}

/// Unknown fields — a FUTURE build's file read by THIS build — parse rather than brick:
/// stray keys at both the top level and inside a record are ignored.
#[test]
fn unknown_fields_are_tolerated() {
    let raw = r#"{
            "backends": [{
                "name": "the CI box",
                "addr": "h:1",
                "observe_key": "K_OBS",
                "control_key": null,
                "control": false,
                "from_the_future": {"nested": true}
            }],
            "active": "the CI box",
            "schema_note": "a v3 field this build has never heard of"
        }"#;
    let f: BackendsFile = serde_json::from_str(raw).expect("future file must parse");
    assert_eq!(f.backends.len(), 1);
    assert_eq!(f.backends[0].name, "the CI box");
    assert_eq!(f.active.as_deref(), Some("the CI box"));
}

/// Missing fields — a v1 file read by a build that has since grown fields — fill with
/// defaults (the `WinSnap::asset_class` idiom): no `control` key loads as UNARMED, no
/// `control_key` as `None`, no `active` as `None`. The default being the DISARMED state is
/// load-bearing: an old file can never arm a write channel by omission.
#[test]
fn a_v1_file_with_missing_fields_loads_disarmed() {
    let raw = r#"{"backends": [{"name": "a", "addr": "h:1", "observe_key": "K"}]}"#;
    let f: BackendsFile = serde_json::from_str(raw).expect("v1 file must parse");
    assert_eq!(f.backends.len(), 1);
    assert!(!f.backends[0].control, "absent `control` must default to DISARMED");
    assert_eq!(f.backends[0].control_key, None);
    assert_eq!(f.active, None);
}

/// An absent file is the ordinary "no backends yet" state — the empty default, not an
/// error. A present-but-corrupt file answers the same default (never brick startup).
#[test]
fn missing_file_is_an_empty_default() {
    let tmp = tempfile::tempdir().expect("temp dir");
    let absent = tmp.path().join(BACKENDS_FILE);
    assert_eq!(load_or_default_from(&absent), BackendsFile::default());

    let corrupt = tmp.path().join("corrupt.json");
    std::fs::write(&corrupt, "{ not json").expect("write corrupt");
    assert_eq!(load_or_default_from(&corrupt), BackendsFile::default());
}

/// The B9 arming gate, all four refuse-paths and the one accept-path: an unarmed record
/// never yields a control key EVEN WHEN the map holds it; `control_key: None` never arms;
/// a named-but-absent key resolves to nothing; and only armed + named + present answers.
#[test]
fn an_unarmed_record_never_yields_a_control_key() {
    let m = vars(&[("PROD2_OBSERVE_KEY", "obs-bytes"), ("PROD2_CONTROL_KEY", "ctl-bytes")]);

    // control = false, key named AND present in the map: still None.
    let disarmed = rec("the CI box");
    assert_eq!(resolve_keys(&disarmed, &m), (Some("obs-bytes"), None));

    // control = true but no control_key named: None.
    let mut keyless = rec("the CI box");
    keyless.control = true;
    keyless.control_key = None;
    assert_eq!(resolve_keys(&keyless, &m), (Some("obs-bytes"), None));

    // control = true, key named, but absent from the map: None (and observe likewise
    // answers only what the map holds).
    let mut armed = rec("the CI box");
    armed.control = true;
    let empty = vars(&[]);
    assert_eq!(resolve_keys(&armed, &empty), (None, None));

    // The one path that arms: control = true + key named + present.
    assert_eq!(resolve_keys(&armed, &m), (Some("obs-bytes"), Some("ctl-bytes")));
}

/// The byte forms the shell sends are exactly [`resolve_keys`]' answer, and an absent key is EMPTY
/// (the node then refuses the handshake, which the caller shows) — never another key, and never a
/// control key on a record that is not armed.
#[test]
fn the_key_bytes_are_resolve_keys_answer_or_empty() {
    let m = vars(&[("PROD2_OBSERVE_KEY", "obs-bytes"), ("PROD2_CONTROL_KEY", "ctl-bytes")]);
    let mut armed = rec("the CI box");
    armed.control = true;
    assert_eq!(observe_key_bytes(&armed, &m), b"obs-bytes");
    assert_eq!(control_key_bytes(&armed, &m), b"ctl-bytes");
    assert!(control_key_bytes(&rec("the CI box"), &m).is_empty(), "a disarmed record signs nothing");
    let empty = vars(&[]);
    assert!(observe_key_bytes(&armed, &empty).is_empty());
    assert!(control_key_bytes(&armed, &empty).is_empty());
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
    let file = BackendsFile { backends: vec![rec(hostile)], active: Some(hostile.to_string()) };
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
/// The two key names `vike-desktop`'s `workspace_credentials` resolves (`vike-app`'s, as this
/// said until 2026-09-28) must be the ones the client actually signs with.
///
/// They are literals on purpose — the settings registry resolves constants crate-wide, so a
/// name imported from `vike-tradehub-client` would be invisible to it and the read would pass
/// its gate by blindness rather than by declaration. That trade is only safe with this test:
/// without it, renaming the constant leaves this file looking up a key nobody sets, and the
/// symptom is an endless `bad mac` at the node.
#[test]
fn observe_and_control_key_names_match_the_client() {
    assert_eq!(OBSERVE_KEY_NAME, vike_tradehub_client::auth::OBSERVE_KEY_ENV);
    assert_eq!(CONTROL_KEY_NAME, vike_tradehub_client::auth::CONTROL_KEY_ENV);
}

/// The DATAHUB half of the same duplication, and the same trade: the constant is a literal so
/// the settings registry can see it, which is only safe with this equality test. A rename on the
/// client side without this would make the desktop's market-data plane look up a key nobody
/// sets, and the symptom is `bad mac` on a reconnect loop.
#[test]
fn the_datahub_observe_key_name_matches_the_client() {
    assert_eq!(super::DATAHUB_OBSERVE_KEY_NAME, vike_hub_proto::auth::DATAHUB_OBSERVE_KEY_ENV);
}

/// A blank record field means the PLATFORM name; a named one overrides it, trimmed.
#[test]
fn a_records_datahub_key_name_defaults_to_the_platform_one() {
    let mut r = rec("the CI box");
    assert_eq!(super::datahub_observe_key_name(&r), super::DATAHUB_OBSERVE_KEY_NAME);
    r.datahub_observe_key = "   ".to_string();
    assert_eq!(
        super::datahub_observe_key_name(&r),
        super::DATAHUB_OBSERVE_KEY_NAME,
        "a whitespace-only override is not an override"
    );
    r.datahub_observe_key = " PROD2_DATAHUB_KEY ".to_string();
    assert_eq!(super::datahub_observe_key_name(&r), "PROD2_DATAHUB_KEY");
}

// The datahub observe key's STORE-backed tests (a stored key resolves, a custom name is the one
// looked up, no datahub dial holds a Write key) live in
// `crates/vike-app-core/tests/hub_key_store_spellings.rs`: they plant `hub_key` rows, and a `src`
// test file that calls a credential writer is a row of `WRITER_CALLERS`
// (`crates/vike-ops/tests/settings_secrets/credential_writer_gate.rs`) this change has no reason to add.
