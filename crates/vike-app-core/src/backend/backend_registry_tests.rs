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
/// The two key names `vike-desktop`'s `workspace_credentials` fills from (`vike-app`'s, as this
/// said until 2026-09-28) must be the ones the client actually signs with.
///
/// They are literals on purpose — the settings registry resolves constants crate-wide, so a
/// name imported from `vike-tradehub-client` would be invisible to it and the read would pass
/// its gate by blindness rather than by declaration. That trade is only safe with this test:
/// without it, renaming the constant leaves this file looking up a key nobody sets, and the
/// symptom is an endless `bad mac` at the node — the exact failure the gap-fill was added to
/// remove.
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
    assert_eq!(super::DATAHUB_OBSERVE_KEY_NAME, vike_node_proto::auth::DATAHUB_OBSERVE_KEY_ENV);
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

/// The ENVIRONMENT is rung one, and it returns before any file is opened — so this case needs
/// no store on disk and touches none. A blank value is not a key.
#[test]
fn the_environment_supplies_the_datahub_observe_key() {
    let env = map(&[(super::DATAHUB_OBSERVE_KEY_NAME, "sekrit")]);
    let (keys, notices) = super::datahub_observe_keys(None, &env, super::DATAHUB_OBSERVE_KEY_NAME);
    assert!(keys.is_some(), "an environment key must resolve without opening a file");
    assert!(notices.is_empty(), "the env rung produces no store notice: {notices:?}");

    for blank in ["", "   ", "\t"] {
        let env = map(&[(super::DATAHUB_OBSERVE_KEY_NAME, blank)]);
        let (keys, _) = super::datahub_observe_keys(None, &env, super::DATAHUB_OBSERVE_KEY_NAME);
        assert!(keys.is_none(), "{blank:?} was treated as a key");
    }
}

/// ⚠ The record's own override NAME is what is looked up, not the platform one — otherwise a
/// per-backend key would silently resolve to whatever the default name happened to hold.
#[test]
fn a_custom_key_name_is_the_one_looked_up() {
    let env = map(&[
        ("PROD2_DATAHUB_KEY", "the-right-one"),
        (super::DATAHUB_OBSERVE_KEY_NAME, "the-wrong-one"),
    ]);
    let (keys, _) = super::datahub_observe_keys(None, &env, "PROD2_DATAHUB_KEY");
    let keys = keys.expect("the override name resolves");
    assert_eq!(
        keys.key_for(vike_node_proto::auth::Scope::Read),
        b"the-right-one",
        "the platform name won over the record's own override"
    );
}

/// ⚠ **No datahub dial resolved in this crate ever holds a Write key, whatever the environment
/// holds** — the store, the chart seed, the venue catalog, the market-data session and Studio's
/// named run all resolve through [`super::datahub_observe_keys`], and its Write half is empty
/// by construction (the map handed to `node_keys_from_vars` carries exactly one entry).
///
/// ⚠ This test was `the_datahub_control_key_is_never_resolved_here` until 2026-09-26, and the
/// NAME stopped being true rather than the assertion: the owner ruled
/// (`docs/decisions/0083-the-runtime-plugin-join-lands.md`, question 1, option (a)) that the
/// desktop DOES resolve the datahub Control key — for Studio's COMPUTE dial alone, through
/// `vike_studio::compute_key_from_vars`, into a type none of these dials accepts. What remains
/// true, and is asserted here, is the narrower rule: THIS crate's dials never see it. So the
/// environment below carries BOTH ways the key can be present — the platform name and Studio's
/// own variable — and every resolver that feeds a dial is asked, including the two dial
/// constructors that wrap it, so a future dial that stopped going through the observe-only
/// resolver would have to leave this test to do it.
///
/// Studio's variable NAME is assembled rather than spelled whole: a whole env-shaped literal in
/// this crate would be a registry-visible read of it here, which is the one thing it must not be.
/// `crates/vike-ops/tests/studio_compute_key_reach_gate.rs` holds the spelling to one Rust file.
#[test]
fn no_datahub_dial_here_resolves_a_write_key_whatever_the_environment_holds() {
    let studio_compute_name = ["VIKE", "STUDIO_COMPUTE_KEY"].join("_");
    let env = map(&[
        (super::DATAHUB_OBSERVE_KEY_NAME, "obs"),
        (vike_node_proto::auth::DATAHUB_CONTROL_KEY_ENV, "ctl"),
        (studio_compute_name.as_str(), "studio-ctl"),
    ]);
    let write = vike_node_proto::auth::Scope::Write;

    let keys = super::datahub_observe_keys(None, &env, super::DATAHUB_OBSERVE_KEY_NAME)
        .0
        .expect("the observe key resolves");
    assert!(
        keys.key_for(write).is_empty(),
        "a datahub CONTROL key reached a desktop datahub dial — that scope compiles \
             client-supplied Rhai and, on the data plane, backfills and deletes"
    );

    let seed = crate::data::chart_seed::SeedDial::resolve(
        "127.0.0.1:1".to_string(),
        None,
        &env,
        super::DATAHUB_OBSERVE_KEY_NAME,
        std::sync::Arc::new(std::sync::Mutex::new(None)),
    );
    let catalog = crate::data::catalog_wire::CatalogDial::resolve(
        "127.0.0.1:1".to_string(),
        None,
        &env,
        super::DATAHUB_OBSERVE_KEY_NAME,
    );
    for (dial, keys) in [("the chart seed", seed.keys), ("the venue catalog", catalog.keys)] {
        let keys = keys.unwrap_or_else(|| panic!("{dial}: the observe key resolves"));
        assert!(keys.key_for(write).is_empty(), "{dial} holds a datahub Write key");
    }
}

use super::{CONTROL_KEY_NAME, NodeKeyFill, OBSERVE_KEY_NAME, fill_node_keys_from_env};

fn map(pairs: &[(&str, &str)]) -> HashMap<String, String> {
    pairs.iter().map(|(k, v)| ((*k).to_string(), (*v).to_string())).collect()
}

/// THE fix: a container that passes the key in the environment and mounts no store now gets a
/// usable key. Before this, the thin image authenticated with an empty one and the node
/// answered `bad mac` for ever — measured on the published 0.1.16 image.
#[test]
fn an_env_key_fills_an_empty_store() {
    let mut creds = map(&[]);
    let got = fill_node_keys_from_env(&mut creds, &map(&[(OBSERVE_KEY_NAME, "sekrit")]));
    assert_eq!(got, NodeKeyFill::ObserveFromEnv);
    assert_eq!(creds.get(OBSERVE_KEY_NAME).map(String::as_str), Some("sekrit"));
}

/// ...and the direction that must NOT happen: a variable on the box silently replacing the key
/// an operator wrote into the credential store.
#[test]
fn a_store_entry_always_wins_over_the_environment() {
    let mut creds = map(&[(OBSERVE_KEY_NAME, "from-store")]);
    let got = fill_node_keys_from_env(&mut creds, &map(&[(OBSERVE_KEY_NAME, "from-env")]));
    assert_eq!(got, NodeKeyFill::Nothing);
    assert_eq!(creds.get(OBSERVE_KEY_NAME).map(String::as_str), Some("from-store"));
}

/// A blank value is no value. Inserting one would trade "no key, here is why" for `bad mac` at
/// the reconnect cadence, which is a strictly worse message for the same broken state.
#[test]
fn a_blank_env_value_is_not_a_key() {
    for blank in ["", "   ", "\t"] {
        let mut creds = map(&[]);
        let got = fill_node_keys_from_env(&mut creds, &map(&[(OBSERVE_KEY_NAME, blank)]));
        assert_eq!(got, NodeKeyFill::Nothing, "{blank:?} was treated as a key");
        assert!(!creds.contains_key(OBSERVE_KEY_NAME));
    }
}

/// Control is never taken from the environment — it places orders — but the attempt is
/// REPORTED, because an asymmetry nobody announces is one every user rediscovers.
#[test]
fn a_control_key_in_the_environment_is_refused_and_reported() {
    let mut creds = map(&[(OBSERVE_KEY_NAME, "s")]);
    let got = fill_node_keys_from_env(&mut creds, &map(&[(CONTROL_KEY_NAME, "control")]));
    assert_eq!(got, NodeKeyFill::ControlIgnored);
    assert!(
        !creds.contains_key(CONTROL_KEY_NAME),
        "the control key reached the credentials map from the environment — that would let a \
             variable on the box arm order placement"
    );
}

/// The ordinary case on a configured box: a store with both keys, nothing from the environment.
#[test]
fn a_complete_store_needs_nothing_from_the_environment() {
    let mut creds = map(&[(OBSERVE_KEY_NAME, "o"), (CONTROL_KEY_NAME, "c")]);
    let before = creds.clone();
    assert_eq!(fill_node_keys_from_env(&mut creds, &map(&[])), NodeKeyFill::Nothing);
    assert_eq!(creds, before);
}
