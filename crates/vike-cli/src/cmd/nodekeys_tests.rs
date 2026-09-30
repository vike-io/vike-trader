use super::*;

fn map(pairs: &[(&str, &str)]) -> HashMap<String, String> {
    pairs.iter().map(|(k, v)| ((*k).to_string(), (*v).to_string())).collect()
}

fn store_path() -> PathBuf {
    PathBuf::from("/p/settings/node.env")
}

/// **THE defect.** Keys only in the node-key store — where `vike-tradehub` itself reads them —
/// must resolve. Before this module the client read `std::env::var` and nothing else, so this
/// configuration produced "nothing to do" and an exit.
#[test]
fn keys_that_exist_only_in_the_credential_store_resolve() {
    let keys = resolve(
        &HashMap::new(),
        &map(&[(OBSERVE_KEY_ENV, "obs-key"), (CONTROL_KEY_ENV, "ctl-key")]),
        Some(&store_path()),
    );
    assert_eq!(keys.observe(), Some(("obs-key", KeyOrigin::Store)));
    assert_eq!(keys.control(), Some(("ctl-key", KeyOrigin::Store)));
    assert!(keys.has_any());
}

/// The chosen precedence, per key independently: an exported value outranks the stored one, and
/// a key present ONLY in the store still comes from the store in the same call.
#[test]
fn the_process_environment_outranks_the_store_per_key() {
    let keys = resolve(
        &map(&[(CONTROL_KEY_ENV, "exported")]),
        &map(&[(OBSERVE_KEY_ENV, "stored-obs"), (CONTROL_KEY_ENV, "stored-ctl")]),
        Some(&store_path()),
    );
    assert_eq!(keys.control(), Some(("exported", KeyOrigin::ProcessEnv)));
    assert_eq!(keys.observe(), Some(("stored-obs", KeyOrigin::Store)));
}

/// A blank value configures nothing — in EITHER source. An empty export must not shadow a real
/// stored key (that is the precedence rule doing damage), and an empty stored value is not a key.
#[test]
fn blank_values_are_absent_in_both_sources() {
    let keys = resolve(
        &map(&[(OBSERVE_KEY_ENV, "   "), (CONTROL_KEY_ENV, "")]),
        &map(&[(OBSERVE_KEY_ENV, "stored-obs")]),
        Some(&store_path()),
    );
    assert_eq!(
        keys.observe(),
        Some(("stored-obs", KeyOrigin::Store)),
        "a whitespace-only export must fall through to the store, not mask it"
    );
    assert_eq!(keys.control(), None);

    let nothing = resolve(&map(&[(CONTROL_KEY_ENV, "  ")]), &HashMap::new(), None);
    assert!(!nothing.has_any());
}

/// Values are TRIMMED, because the server's `auth::from_vars` trims: an untrimmed client
/// Both names must be the ones the SERVER reads, and until now only one of them was held to
/// that.
///
/// The four crate-local copies of these two names are duplicated ON PURPOSE — the settings
/// registry resolves constants crate-wide, so a name imported from `vike-tradehub-client`
/// would be invisible to it and this crate's read would pass its gate by BLINDNESS rather than
/// by declaration. `vike-app-core`'s `observe_and_control_key_names_match_the_client` states
/// that trade and pays for it with an equality assertion. This is the same payment for
/// `vike-cli`, and it was missing.
///
/// ⚠ What it closes: `values_are_trimmed_to_match_the_servers_own_reader` below happens to
/// cover the CONTROL half — it feeds the server's own reader a map keyed on our
/// `CONTROL_KEY_ENV` and unwraps — but that is a side effect of a TRIMMING test, not a gate,
/// and it leaves OBSERVE covered by nothing at all. A drifted `OBSERVE_KEY_ENV` would look up a
/// key nobody sets, and the symptom is an endless `bad mac` at the node on every `trade` verb
/// and on `mcp` — with every test in this crate green.
#[test]
fn both_key_names_match_the_servers_own() {
    assert_eq!(OBSERVE_KEY_ENV, vike_tradehub_client::auth::OBSERVE_KEY_ENV);
    assert_eq!(CONTROL_KEY_ENV, vike_tradehub_client::auth::CONTROL_KEY_ENV);
}

/// signs with different bytes than the server verifies and the handshake fails opaquely.
#[test]
fn values_are_trimmed_to_match_the_servers_own_reader() {
    let keys = resolve(&HashMap::new(), &map(&[(CONTROL_KEY_ENV, "  ctl-key\n")]), None);
    assert_eq!(keys.control(), Some(("ctl-key", KeyOrigin::Store)));

    // …and that is byte-for-byte what the SERVER would load from the same map, so the two
    // sides cannot disagree about the key that signs the handshake.
    let server = vike_tradehub_client::auth::from_vars(&map(&[(CONTROL_KEY_ENV, "  ctl-key\n")]))
        .expect("one key present");
    assert_eq!(
        server.key_for(vike_tradehub_client::proto::Scope::Write),
        keys.control().unwrap().0.as_bytes()
    );
}

/// The absent-keys message NAMES the store it looked in — the diagnostic the old
/// "not set in the environment" line could not give, and the reason the defect was hard to
/// self-diagnose.
#[test]
fn the_absent_message_names_both_sources_and_the_store_path() {
    let keys = resolve(&HashMap::new(), &HashMap::new(), Some(&store_path()));
    let msg = keys.absent_message("trade");
    assert!(msg.contains("vike-cli trade:"), "{msg}");
    assert!(msg.contains(OBSERVE_KEY_ENV) && msg.contains(CONTROL_KEY_ENV), "{msg}");
    assert!(msg.contains("process environment"), "{msg}");
    assert!(msg.to_lowercase().contains("node.env"), "must name the store file: {msg}");
    assert!(msg.contains("node-key store"), "{msg}");

    // No project above the working directory: say THAT, rather than naming a path that is not
    // the one anything would read.
    let rootless = resolve(&HashMap::new(), &HashMap::new(), None);
    let msg = rootless.absent_message("mcp");
    assert!(msg.contains("vike-cli mcp:"), "{msg}");
    assert!(msg.contains("NOT resolved"), "{msg}");
    assert!(msg.contains("VIKE_SETTINGS_DIR"), "must say how to point at one: {msg}");
}

/// The observe-specific message (a READ verb's absent-key error): it names the observe key
/// and the store, and — the case [`NodeKeyring::absent_message`] gets WRONG for that surface —
/// when a control key resolved it says so and says why it cannot substitute, instead of
/// claiming no key was found.
#[test]
fn the_observe_absent_message_names_the_key_and_disclaims_a_resolved_control_key() {
    // No key at all: both sources named, observe key named, store path named.
    let none = resolve(&HashMap::new(), &HashMap::new(), Some(&store_path()));
    let msg = none.observe_absent_message("trade status");
    assert!(msg.contains("vike-cli trade status:"), "{msg}");
    assert!(msg.contains(OBSERVE_KEY_ENV), "{msg}");
    assert!(msg.contains("process environment"), "{msg}");
    assert!(msg.to_lowercase().contains("node.env"), "must name the store file: {msg}");
    assert!(!msg.contains("control key DID resolve"), "no control key exists here: {msg}");

    // A control key resolved: the message must not pretend nothing was found — it says the
    // control key cannot authenticate a read.
    let ctl_only =
        resolve(&map(&[(CONTROL_KEY_ENV, "ctl-key")]), &HashMap::new(), Some(&store_path()));
    let msg = ctl_only.observe_absent_message("trade status");
    assert!(msg.contains("control key DID resolve"), "{msg}");
    assert!(!msg.contains("ctl-key"), "a key value leaked into a user message: {msg}");

    // No project above the working directory: same NOT-resolved wording as the either-key
    // message, pointing at VIKE_SETTINGS_DIR.
    let rootless = resolve(&HashMap::new(), &HashMap::new(), None);
    let msg = rootless.observe_absent_message("trade status");
    assert!(msg.contains("NOT resolved") && msg.contains("VIKE_SETTINGS_DIR"), "{msg}");
}

/// A keyring never renders a key, in any formatter, from any source.
#[test]
fn debug_redacts_every_key() {
    let keys = resolve(
        &map(&[(CONTROL_KEY_ENV, "SUPER-SECRET-CTL")]),
        &map(&[(OBSERVE_KEY_ENV, "SUPER-SECRET-OBS")]),
        Some(&store_path()),
    );
    let rendered = format!("{keys:?}");
    assert!(!rendered.contains("SUPER-SECRET"), "a key reached Debug: {rendered}");
    assert!(rendered.contains("process env") && rendered.contains("node-key store"));
}
