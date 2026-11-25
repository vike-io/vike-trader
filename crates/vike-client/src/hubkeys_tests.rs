use super::*;

fn map(pairs: &[(&str, &str)]) -> HashMap<String, String> {
    pairs.iter().map(|(k, v)| ((*k).to_string(), (*v).to_string())).collect()
}

fn store_path() -> PathBuf {
    PathBuf::from("/p/settings/db/vike.db")
}

/// The bytes a shared credential of the tests' seam signs with, or `None` for a device key.
fn shared(resolved: Option<(Signer<'_>, KeyOrigin)>) -> Option<(Vec<u8>, KeyOrigin)> {
    match resolved? {
        (Signer::Shared(bytes), origin) => Some((bytes.to_vec(), origin)),
        (Signer::Device(_), _) => None,
    }
}

/// A device key serves BOTH halves (the node's role decides), says it is one, and is no shared key.
#[test]
fn a_device_key_serves_both_halves() {
    let (device, _) = DeviceKey::generate().expect("a device key");
    let keys = HubKeyring::of_device(Some(device.clone()), Some(&store_path()));
    for half in [keys.observe(), keys.control()] {
        match half {
            Some((Signer::Device(held), KeyOrigin::Device)) => {
                assert_eq!(held.key_id(), device.key_id())
            }
            other => panic!("{other:?}"),
        }
    }
    assert!(keys.has_any());
    assert!(!HubKeyring::of_device(None, Some(&store_path())).has_any());
}

/// The tests' seam: shared bytes per name, independently; a blank value is absent.
#[test]
fn the_seam_reads_its_two_names_and_a_blank_value_is_absent() {
    let keys = resolve(
        &map(&[(OBSERVE_KEY_ENV, "stored-obs"), (CONTROL_KEY_ENV, "   ")]),
        Some(&store_path()),
    );
    assert_eq!(shared(keys.observe()), Some((b"stored-obs".to_vec(), KeyOrigin::Store)));
    assert!(keys.control().is_none());

    let nothing = resolve(&map(&[(CONTROL_KEY_ENV, "  ")]), None);
    assert!(!nothing.has_any());
}

/// Values are TRIMMED: a trailing newline from a pipe is the same key.
#[test]
fn seam_values_are_trimmed() {
    let keys = resolve(&map(&[(CONTROL_KEY_ENV, "  ctl-key\n")]), None);
    assert_eq!(shared(keys.control()), Some((b"ctl-key".to_vec(), KeyOrigin::Store)));
}

/// The absent message NAMES the store it looked in and how this box gets a device key, and names no shared pair
/// and no environment as a source.
#[test]
fn the_absent_message_names_the_store_and_the_device_key_remedy() {
    use vike_hub_protocol::conn::no_device_key_cure;
    let keys = HubKeyring::of_device(None, Some(&store_path()));
    let msg = keys.absent_message("trade", "127.0.0.1:9301");
    assert!(msg.contains("vike-cli trade:"), "{msg}");
    assert!(msg.contains(DEVICE_KEY_WORDS), "{msg}");
    assert!(msg.contains("vike.db"), "must name the store file: {msg}");
    assert!(msg.contains(no_device_key_cure(true)), "this box's hubs: {msg}");
    // A laptop dialling a server DIRECTLY is never told to run a server verb on itself.
    let remote = keys.absent_message("trade", "the CI box.lan:9301");
    assert!(remote.contains(no_device_key_cure(false)), "another server: {remote}");
    assert!(!remote.contains(no_device_key_cure(true)), "{remote}");
    assert!(!msg.contains(OBSERVE_KEY_ENV) && !msg.contains(CONTROL_KEY_ENV), "{msg}");
    for env_remedy in ["environment", "export", "Environment="] {
        assert!(!msg.contains(env_remedy), "the message offers `{env_remedy}` as a source: {msg}");
    }

    // No project above the working directory: say THAT, rather than naming a path that is not
    // the one anything would read, and offer no new key for one that may well exist.
    let rootless = HubKeyring::of_device(None, None);
    let msg = rootless.absent_message("trade", "127.0.0.1:9301");
    assert!(msg.contains("vike-cli trade:"), "{msg}");
    assert!(msg.contains("resolved no settings directory"), "{msg}");
    assert!(msg.contains("VIKE_SETTINGS_DIR"), "must say how to point at one: {msg}");
    // The whole body is the no-directory refusal, which names no key to mint.
    let body = crate::refusals::DeviceKeyRefusal::NoSettingsDirectory.rendered();
    assert!(msg.ends_with(&body), "{msg}");
}

/// **The one body every door answers**, by its id: the CLI's messages open with the command and carry it
/// rendered, `vike-mcp` answers it whole. A store that is there and lacks the row says so, one that is not says
/// absent, and no node named reads as this box's own hubs.
#[test]
fn the_absent_refusal_is_one_body_under_its_id() {
    use crate::refusals::DeviceKeyRefusalId;
    use vike_hub_protocol::conn::no_device_key_cure;
    let keys = HubKeyring::of_device(None, Some(&store_path()));
    let absent = keys.absent_refusal(None);
    assert_eq!(absent.id(), DeviceKeyRefusalId::NotInStore);
    assert!(absent.to_string().contains("/p/settings/db/vike.db: absent"), "{absent}");
    assert!(absent.to_string().contains(no_device_key_cure(true)), "no node = this box: {absent}");
    assert!(
        keys.absent_message("trade", "127.0.0.1:9301").ends_with(&absent.rendered()),
        "the CLI carries the body whole"
    );
    let rootless = HubKeyring::of_device(None, None).absent_refusal(Some("the CI box.lan:9301"));
    assert_eq!(rootless.id(), DeviceKeyRefusalId::NoSettingsDirectory);
    assert!(rootless.rendered().starts_with("error [dial.device_key.no_settings_directory]: "));
}

/// The observe-specific message: it names the device key and the store, and when a control credential resolved
/// alone (the seam) it says why that cannot substitute.
#[test]
fn the_observe_absent_message_names_the_key_and_disclaims_a_resolved_control_key() {
    let none = HubKeyring::of_device(None, Some(&store_path()));
    let msg = none.observe_absent_message("trade status", "the CI box.lan:9301");
    assert!(msg.contains("vike-cli trade status:"), "{msg}");
    assert!(msg.contains(vike_hub_protocol::conn::no_device_key_cure(false)), "{msg}");
    assert!(msg.contains(DEVICE_KEY_WORDS), "{msg}");
    assert!(!msg.contains("environment"), "the environment is not a source: {msg}");
    assert!(msg.contains("vike.db"), "must name the store file: {msg}");
    assert!(!msg.contains("control key DID resolve"), "no control key exists here: {msg}");

    let control_only = resolve(&map(&[(CONTROL_KEY_ENV, "ctl-key")]), Some(&store_path()));
    let msg = control_only.observe_absent_message("trade status", "127.0.0.1:9301");
    assert!(msg.contains("control key DID resolve"), "{msg}");
    assert!(!msg.contains("ctl-key"), "a key value leaked into a user message: {msg}");

    let rootless = HubKeyring::of_device(None, None);
    let msg = rootless.observe_absent_message("trade status", "127.0.0.1:9301");
    assert!(
        msg.contains("resolved no settings directory") && msg.contains("VIKE_SETTINGS_DIR"),
        "{msg}"
    );
}

/// A failed node READ is sorted once for both doors: an old node and a refused key say asking again cannot
/// succeed, a socket names the address, and the first line opens with the door's own `who`.
#[test]
fn a_failed_node_read_is_sorted_once_under_the_doors_opening() {
    use vike_hub_protocol::call::HubCallError;
    let old = HubCallError::NotOffered {
        feature: "strategy-verbs".into(),
        text: "no strategy-verbs".into(),
    };
    let lines = node_read_failure_lines("strategy_status", "n:1", "this verb", &old);
    assert_eq!(lines[0], "strategy_status: no strategy-verbs");
    assert!(
        lines[1].contains("upgrade the node at n:1") && lines[1].contains("cannot succeed"),
        "{lines:?}"
    );
    let denied = HubCallError::AccessDenied("bad mac".into());
    assert_eq!(
        node_read_failure_lines("vike-cli trade status", "n:1", "x", &denied),
        observe_permission_denied_lines("trade status", "n:1", &denied),
        "one wording of a refused key"
    );
    let down = HubCallError::NotSent(std::io::Error::other("refused"));
    assert_eq!(
        node_read_failure_lines("t", "n:1", "x", &down),
        ["t: cannot query the node at n:1: refused"]
    );
}

/// A keyring never renders a key, in any formatter.
#[test]
fn debug_redacts_every_key() {
    let keys = resolve(
        &map(&[(CONTROL_KEY_ENV, "SUPER-SECRET-CTL"), (OBSERVE_KEY_ENV, "SUPER-SECRET-OBS")]),
        Some(&store_path()),
    );
    let rendered = format!("{keys:?}");
    assert!(!rendered.contains("SUPER-SECRET"), "a key reached Debug: {rendered}");
    assert!(rendered.contains("hub-key store"), "{rendered}");

    let (device, _) = DeviceKey::generate().expect("a device key");
    let rendered = format!("{:?}", HubKeyring::of_device(Some(device), None));
    assert!(rendered.contains("device key"), "{rendered}");
}

/// A keyring resolved without the settings dials with the window every dial had before the patience rows existed,
/// and the boot's resolved windows replace it as given — raised, never clamped here (the bounds are `vike-config`'s).
#[test]
fn a_keyring_carries_the_box_patience_and_defaults_to_todays_windows() {
    use vike_tradehub_client::liveness::LinkPatience;
    assert_eq!(HubKeyring::default().patience(), LinkPatience::default());
    assert_eq!(HubKeyring::of_device(None, None).patience(), LinkPatience::default());
    let raised = LinkPatience::from_millis(120_000, 90_000);
    let keys = HubKeyring::of_device(None, None).with_patience(raised);
    assert_eq!(keys.patience(), raised);
}
