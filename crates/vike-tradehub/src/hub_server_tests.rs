//! [`open_observe_door`]'s non-loopback refusal, as the operator reads it, and [`access_list`]'s empty-list probe.

use super::*;

/// **A list with no row is not held, so the shared handshake refuses every device as "no device registered"**
/// (decision 0288 item 8). The wire refusal is
/// `crates/vike-hub-protocol/src/handshake_tests.rs`'s `a_device_authenticates_by_the_access_list_and_is_refused_unadmitted_or_once_revoked`,
/// the probe's first-row flip `crates/vike-secrets/tests/store/access.rs`'s
/// `the_list_is_held_from_its_first_row_and_a_named_device_key_is_its_own`; this pins that the tradehub's own list
/// carries the probe. Read-only on purpose: a temp directory holding no store writes nothing (this file is no store
/// writer).
#[test]
fn the_tradehub_list_over_a_settings_dir_with_no_row_is_not_held() {
    let dir = tempfile::tempdir().expect("a temp settings dir");
    let list = access_list(dir.path().to_path_buf());
    assert!(!list.held(), "no row: every device is refused as `no device registered`");
    assert!(list.entry("nk-1").is_none());
}

/// **A wildcard `config.tradehub_addr` with no consent opens no door, and its one ERROR line names
/// the cure** (decision 0280, the owner's ruling of 2026-11-12). The container image consents for
/// nobody now, so this line is the whole of what a `docker run` operator learns at start. It returns
/// before any key read or socket, so the test needs neither.
#[test]
fn a_public_address_is_refused_without_the_consent_and_the_refusal_names_the_cure() {
    let (door, events) = vike_log::capture::captured(|| {
        open_observe_door(
            Some("0.0.0.0:17879"),
            true,
            false,
            None,
            None,
            None,
            crate::publish::ObserverStream::default(),
        )
    });
    assert!(door.is_none(), "no consent, no door");
    let errors: Vec<_> = events.iter().filter(|e| e.level == tracing::Level::ERROR).collect();
    assert_eq!(errors.len(), 1, "one refusal line: {events:?}");
    let refusal = errors[0];
    assert_eq!(refusal.field("addr"), Some("0.0.0.0:17879"));
    for words in [
        "NOT loopback — observe server NOT started",
        "To open trading to the network on purpose, start the container with \
         `--allow-public-bind` and firewall the port to your own IP. Otherwise connect with \
         `vike-cli backend connect HOST --server-project DIR` over ssh.",
        "`vike-cli config set flags.tradehub_allow_public_bind true`",
    ] {
        assert!(refusal.message.contains(words), "{words:?} is missing from: {}", refusal.message);
    }
}
