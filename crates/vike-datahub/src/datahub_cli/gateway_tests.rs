//! The datahub's gatewayhub link: the address off the settings, the CLIENT pair off the hub key store,
//! and the degraded (unsigned-refusing) link when the pair is not there.
//!
//! Every store is a real settings database in a temp directory the test made, with fake key values
//! planted by the writer's record-less `test-support` body (the `hub_key` table's own writer; no
//! test here reaches a real settings directory). The log is read with `vike_log::capture`, which sees
//! the events of THIS thread, and no assertion lets a key VALUE appear in it.

use std::path::Path;

use vike_hub_proto::auth::Scope;

use super::{client_keys, gateway_link, pair_names};

/// The value of every key a service OTHER than the gatewayhub owns that this datahub could hold:
/// the tradehub's pair and admin key, and the datahub's own pair.
const OTHER_SERVICES_VALUE: &str = "a-key-of-another-service";
/// The gatewayhub's observe key, as these tests plant it.
const GW_OBSERVE_VALUE: &str = "gatewayhub-observe-key-of-this-test";
/// The gatewayhub's control key, as these tests plant it.
const GW_CONTROL_VALUE: &str = "gatewayhub-control-key-of-this-test";

/// The handle the boot would hand this root for a settings directory `dir` (the full scope).
fn store_at(dir: &Path) -> vike_secrets::CredentialStore {
    vike_secrets::CredentialStore::open(
        Some(vike_secrets::StoreLocation::new(dir)),
        vike_secrets::CredentialScope::Full,
    )
}

/// The handle a boot that resolved NO settings directory hands this root: no location.
fn nowhere() -> vike_secrets::CredentialStore {
    vike_secrets::CredentialStore::open(None, vike_secrets::CredentialScope::Full)
}

/// A settings database with no rows at all: the store a project has before any key is minted.
fn planted() -> tempfile::TempDir {
    let dir = tempfile::TempDir::new().expect("a temp settings dir");
    vike_secrets::plant_settings_rows(dir.path(), &vike_secrets::StoredSettings::default())
        .expect("plant the store");
    dir
}

/// `(name, value)` rows into the `hub_key` table of the store [`planted`] made.
fn plant_hub_keys(dir: &Path, rows: &[(&str, &str)]) {
    let updates: Vec<(String, String)> =
        rows.iter().map(|(name, value)| ((*name).to_string(), (*value).to_string())).collect();
    vike_secrets::save_credentials_to_store_unjournalled(
        dir,
        vike_secrets::Table::HubKey,
        &updates,
        vike_secrets::UpsertIntent::Add,
    )
    .expect("plant the hub keys");
}

/// Every name that is NOT the gatewayhub's pair: the tradehub's pair and admin key, THE DATAHUB'S OWN pair
/// and strategy-lane key, and the gatewayhub's OWN Trade and Admin keys (`[7]`, `[8]`: the family is
/// wider than the pair, and this client must not hold the order-scope key by that filing), each under
/// [`OTHER_SERVICES_VALUE`].
fn other_services_keys() -> Vec<(&'static str, &'static str)> {
    let pair = pair_names();
    vike_venues::credential_keys::PLATFORM_KEYS
        .iter()
        .filter(|name| !pair.contains(*name))
        .map(|name| (*name, OTHER_SERVICES_VALUE))
        .collect()
}

/// Settings naming `addr` as the gatewayhub's address (or none).
fn settings_with(addr: Option<&str>) -> vike_config::Settings {
    let mut settings = vike_config::Settings::default();
    settings.config.gatewayhub_addr = addr.map(str::to_string);
    settings
}

/// The log lines of `events` at `level`, as their messages.
fn at_level(events: &[vike_log::capture::CapturedEvent], level: tracing::Level) -> Vec<&str> {
    events.iter().filter(|e| e.level == level).map(|e| e.message.as_str()).collect()
}

/// The address is the `config.gatewayhub_addr` row, else the compiled default.
#[test]
fn the_address_is_the_row_or_the_compiled_default() {
    let store = nowhere();
    assert_eq!(
        gateway_link(&settings_with(Some("127.0.0.1:7999")), &store).addr(),
        "127.0.0.1:7999"
    );
    assert_eq!(
        gateway_link(&settings_with(None), &store).addr(),
        vike_config::config::DEFAULT_GATEWAYHUB_ADDR
    );
}

/// **The link carries the gatewayhub CLIENT pair and nothing of another service's**: with every
/// platform key in the store - the tradehub's pair and admin key, the datahub's OWN pair, and the
/// gatewayhub's pair - the observe key signs `Read`, the control key signs `Write`, `Account` has no
/// key, and the bytes are the gatewayhub's two values, never the datahub's own. No value reaches
/// the log.
#[test]
fn the_link_carries_the_gatewayhub_client_pair_and_not_the_datahubs_own() {
    let [observe, control] = pair_names();
    let dir = planted();
    let mut rows = other_services_keys();
    rows.push((observe, GW_OBSERVE_VALUE));
    rows.push((control, GW_CONTROL_VALUE));
    plant_hub_keys(dir.path(), &rows);
    let store = store_at(dir.path());

    let keys = client_keys(&store).keys;
    assert_eq!(keys.key_for(Scope::Read), GW_OBSERVE_VALUE.as_bytes());
    assert_eq!(keys.key_for(Scope::Write), GW_CONTROL_VALUE.as_bytes());
    assert!(
        !keys.has(Scope::Account),
        "the datahub holds no admin key for the gatewayhub: {keys:?}"
    );
    assert!(!keys.has(Scope::Trade), "the datahub holds no order-scope key: {keys:?}");

    let (link, events) = vike_log::capture::captured(|| gateway_link(&settings_with(None), &store));
    assert!(link.can_sign(Scope::Read) && link.can_sign(Scope::Write));
    assert!(!link.can_sign(Scope::Account));
    assert!(
        at_level(&events, tracing::Level::WARN)
            .iter()
            .all(|m| !m.contains("no gatewayhub client key")),
        "a provisioned box is not told the pair is missing: {events:?}"
    );
    for value in [GW_OBSERVE_VALUE, GW_CONTROL_VALUE, OTHER_SERVICES_VALUE] {
        assert!(
            !events
                .iter()
                .any(|e| e.message.contains(value) || e.fields.values().any(|v| v.contains(value))),
            "a key value reached the log: {events:?}"
        );
    }
}

/// **No pair, no signature - and nothing stands in for it.** A box that has not provisioned the
/// pair gets a link that can sign nothing (every remote call then names the missing key), whatever
/// else the store holds: no settings directory, a directory without a database, a database without
/// hub keys, and a database holding EVERY OTHER service's keys - the datahub's own pair included,
/// which must not be borrowed. The datahub does not crash; it says so once, with the remedy.
#[test]
fn a_link_without_the_pair_cannot_sign_and_nothing_stands_in() {
    let no_database = tempfile::TempDir::new().expect("a temp dir");
    let empty = planted();
    let others = planted();
    plant_hub_keys(others.path(), &other_services_keys());

    let cases = [
        ("no location", nowhere()),
        ("a directory without a database", store_at(no_database.path())),
        ("a database without hub keys", store_at(empty.path())),
        ("a database with only other services' keys", store_at(others.path())),
    ];
    for (what, store) in &cases {
        let (link, events) =
            vike_log::capture::captured(|| gateway_link(&settings_with(None), store));
        for scope in [Scope::Read, Scope::Write, Scope::Account] {
            assert!(!link.can_sign(scope), "{what}: the link must not sign {scope:?}");
        }
        let said: Vec<&str> = at_level(&events, tracing::Level::WARN)
            .into_iter()
            .filter(|m| m.contains("no gatewayhub client key"))
            .collect();
        assert_eq!(said.len(), 1, "{what}: the missing pair is said once: {events:?}");
        assert!(said[0].contains("vike-cli gatewayhub setup"), "{what}: the remedy: {}", said[0]);
        assert!(!said[0].contains(OTHER_SERVICES_VALUE), "{what}: a value in the line");
    }
}

/// One key signs one scope: the scope without a key is refused locally, per call, and the other
/// scope's lanes keep working.
#[test]
fn one_key_signs_one_scope() {
    let [observe, control] = pair_names();
    let only_observe = planted();
    plant_hub_keys(only_observe.path(), &[(observe, GW_OBSERVE_VALUE)]);
    let link = gateway_link(&settings_with(None), &store_at(only_observe.path()));
    assert!(link.can_sign(Scope::Read) && !link.can_sign(Scope::Write));

    let only_control = planted();
    plant_hub_keys(only_control.path(), &[(control, GW_CONTROL_VALUE)]);
    let link = gateway_link(&settings_with(None), &store_at(only_control.path()));
    assert!(!link.can_sign(Scope::Read) && link.can_sign(Scope::Write));
}

/// **A store that EXISTS and will not read is an ERROR, never "no keys".** Bytes that are not a
/// database where the database belongs (`chmod 000` proves nothing as root, which CI is): the cause
/// is logged once at `error`, in words that say it is not the same as the pair being absent, and the
/// link degrades to the unsigned one rather than crashing the datahub.
#[test]
fn an_unreadable_store_is_an_error_and_degrades_to_an_unsigned_link() {
    let dir = tempfile::TempDir::new().expect("a temp dir");
    std::fs::create_dir_all(dir.path().join("db")).expect("the db directory");
    std::fs::write(vike_secrets::db_path_in(dir.path()), b"not a sqlite database at all")
        .expect("plant the garbage");
    let store = store_at(dir.path());

    let (link, events) = vike_log::capture::captured(|| gateway_link(&settings_with(None), &store));
    assert!(!link.can_sign(Scope::Read) && !link.can_sign(Scope::Write));
    let errors = at_level(&events, tracing::Level::ERROR);
    assert_eq!(errors.len(), 1, "the unreadable store is logged, once: {events:?}");
    assert!(errors[0].contains("NOT the same as the pair being absent"), "{}", errors[0]);
    // ...and the ABSENT-pair warning is not said after it: it would call an unreadable store an empty
    // one and send the operator to mint a pair they may already own.
    assert!(
        at_level(&events, tracing::Level::WARN).is_empty(),
        "the unreadable store must not be followed by the absent-pair warning: {events:?}"
    );
    assert!(
        events.iter().all(|e| !e.message.contains("no gatewayhub client key")
            && !e.message.contains("Mint the pair")),
        "no event may tell the operator to mint a pair: {events:?}"
    );
    // The result says why, so the caller can word it: unreadable, not absent.
    let read = client_keys(&store);
    assert!(read.store_unreadable && !read.keys.has(Scope::Read) && !read.keys.has(Scope::Write));
}

/// The absent pair is NOT the unreadable store: `client_keys` says so, so the caller's "absent"
/// warning is only ever said for an absent pair.
#[test]
fn an_absent_pair_is_not_an_unreadable_store() {
    let empty = planted();
    let read = client_keys(&store_at(empty.path()));
    assert!(!read.store_unreadable, "a readable store without the pair is absent, not unreadable");
    assert!(!read.keys.has(Scope::Read) && !read.keys.has(Scope::Write));
}

/// ⚠ The pair is read out of `PLATFORM_KEYS` by INDEX, so this is the assertion that the indexing is
/// right: both names are the gatewayhub's, observe first and control second, this daemon's CLIENT
/// predicate admits them, and neither is a name the datahub or the tradehub SERVES under.
#[test]
fn the_pair_names_are_the_gatewayhubs_and_not_the_datahubs_own() {
    let [observe, control] = pair_names();
    assert!(observe.contains("GATEWAYHUB") && observe.contains("OBSERVE"), "{observe}");
    assert!(control.contains("GATEWAYHUB") && control.contains("CONTROL"), "{control}");
    assert_ne!(observe, control);
    for name in [observe, control] {
        assert_eq!(
            vike_venues::credential_keys::platform_key_service(name),
            Some(vike_venues::credential_keys::GATEWAYHUB_SERVICE)
        );
        assert!(vike_venues::credential_keys::is_datahub_gatewayhub_client_key(name), "{name}");
        assert!(!vike_venues::credential_keys::is_datahub_pair_key(name), "{name}");
        assert!(!vike_venues::credential_keys::is_tradehub_daemon_key(name), "{name}");
    }
}

/// **The link never reaches for the datahub's own pair**, in text: the file's code (comments
/// removed) names none of the predicates or constants of the pair this daemon SERVES under, nor the
/// wide table. The behaviour half is `a_link_without_the_pair_cannot_sign_and_nothing_stands_in`; this
/// is the half that holds if a later edit "helpfully" falls back.
#[test]
fn the_link_never_reaches_for_the_datahubs_own_pair() {
    let source = include_str!("gateway.rs");
    let code: String =
        source.lines().filter(|l| !l.trim_start().starts_with("//")).collect::<Vec<_>>().join("\n");
    // Only the code above the test module declaration is the link's.
    let code = &code[..code.find("#[cfg(test)]").expect("the test module declaration")];
    for forbidden in [
        "is_datahub_pair_key",
        "is_datahub_daemon_key",
        "is_platform_key",
        "is_tradehub",
        "hub_keys_from_vars",
        "DATAHUB_OBSERVE_KEY_ENV",
        "DATAHUB_CONTROL_KEY_ENV",
        "read_hub_keys",
    ] {
        assert!(!code.contains(forbidden), "gateway.rs must not name `{forbidden}`");
    }
    assert!(code.contains("is_datahub_gatewayhub_client_key"), "the one predicate it reads with");
}
