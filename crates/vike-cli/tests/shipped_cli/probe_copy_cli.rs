//! **`vike-cli backend probe-copy` keeps every device ROW with a THROWAWAY key, and no invitation** — the shipped
//! binary, the copy `deploy/docker/vikectl`'s `probe_copy` makes (decision 0288: a private key never leaves its
//! device).
//!
//! Over a store holding the Telegram channel's device (`owner/telegram`, which the channel refuses its start without),
//! a second device, an invitation and Polymarket's proxy URL (a SECRET venue field, written by `config set`, which
//! stores the field UPPER-case, and once more in a lower-case row): the copy holds both device rows,
//! each with a key its reader takes (`vike_hub_protocol::access::DeviceKey::from_pkcs8`, as
//! `vike_client::resolved::device_key_of` reads it) under the key id of that new key, none the live key, and no
//! invitation; no live private key, invitation secret or proxy URL is in the copy's bytes (the verb names the secret
//! venue fields itself).

use vike_hub_protocol::access::DeviceKey;

use crate::common;

const INVITE_SECRET: &[u8] = b"invite-auth-secret-4b5c";
/// Polymarket's proxy URL, a SECRET venue field (`vike_venues::venue_fields::VENUE_FIELDS`): it carries a password.
const PROXY: &str = "socks5://probe-user:proxy-password-8e9f@<host>:1080";
/// The same field in a row spelled lower-case, as the vocabulary spells it.
const PROXY_LOWER: &str = "socks5://probe-user:proxy-password-3a4b@<host>:1080";

/// `vike-cli config set venue.polymarket.socks_proxy -` with `url` on stdin: the way an operator stores it.
fn set_proxy(settings: &std::path::Path, user_data: &std::path::Path, url: &str) {
    use std::io::Write as _;
    use std::process::{Command, Stdio};
    let mut child = Command::new(env!("CARGO_BIN_EXE_vike-cli"))
        .args(["config", "set", "venue.polymarket.socks_proxy", "-"])
        .env("VIKE_SETTINGS_DIR", settings)
        .env("VIKE_USER_DATA_DIR", user_data)
        .env_remove("VIKE_MAX_ORDER_NOTIONAL")
        .env_remove("VIKE_TRADEHUB_MAX_ORDER_NOTIONAL")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn vike-cli");
    child.stdin.take().expect("stdin").write_all(format!("{url}\n").as_bytes()).expect("pipe");
    let out = child.wait_with_output().expect("vike-cli ends");
    assert!(out.status.success(), "config set: {}", String::from_utf8_lossy(&out.stderr));
}

#[test]
fn a_device_row_survives_with_a_throwaway_key_and_no_invitation_does() {
    let dir = tempfile::tempdir().expect("tempdir");
    let live = dir.path().join("live/settings");
    std::fs::create_dir_all(&live).expect("the live settings directory");
    common::seed_store(&live, &[]);
    let mut held = Vec::new();
    for device in ["telegram", "server"] {
        let (key, document) = DeviceKey::generate().expect("a live key");
        let id = key.key_id();
        vike_secrets::access::device_key_insert(&live, &id, "owner", device, &document)
            .expect("a device row");
        held.push((id, document));
    }
    vike_secrets::access::add_access_invite(
        &live,
        "0a1b2c3d",
        "alice",
        "observe",
        INVITE_SECRET,
        i64::MAX,
    )
    .expect("an invitation");
    // The proxy as an operator stores it (the field UPPER-cased by `config set`), and once more lower-case.
    set_proxy(&live, dir.path(), PROXY);
    let stored = std::fs::read(vike_secrets::db_path_in(&live)).expect("the live store's bytes");
    assert!(
        stored.windows(11).any(|w| w == b"SOCKS_PROXY"),
        "config set stores the field upper-case"
    );
    vike_secrets::plant_venue_setting_for_test(
        &live,
        "polymarket",
        None,
        "socks_proxy",
        PROXY_LOWER,
    )
    .expect("a secret venue field spelled lower-case, as hand-typed SQL may store it");

    let into = dir.path().join("probe/settings");
    let into_arg = into.to_str().expect("a UTF-8 scratch path");
    let out = common::run_in(&live, dir.path(), &["backend", "probe-copy", "--into", into_arg]);
    assert!(out.status.success(), "probe-copy: {}", String::from_utf8_lossy(&out.stderr));

    let bytes = std::fs::read(vike_secrets::db_path_in(&into)).expect("the copy's bytes");
    let live_only = [INVITE_SECRET, PROXY.as_bytes(), PROXY_LOWER.as_bytes()];
    for secret in held.iter().map(|(_, document)| document.as_slice()).chain(live_only) {
        assert!(!bytes.windows(secret.len()).any(|w| w == secret), "a live secret is in the copy");
    }
    let row = vike_secrets::access::device_key_named(&into, "owner", "telegram")
        .expect("read the copy")
        .expect("the Telegram channel's row is kept");
    let key = DeviceKey::from_pkcs8(&row.private_key).expect("a key its reader takes");
    assert_eq!(key.key_id(), row.key_id, "the row names its new key");
    assert!(held.iter().all(|(id, _)| *id != row.key_id), "a throwaway, not the live key");
    let other = vike_secrets::access::device_key_named(&into, "owner", "server")
        .expect("read the copy")
        .expect("the other row is kept");
    assert_ne!(other.key_id, row.key_id, "each row gets its own key");
    assert!(
        vike_secrets::access::list_access_invites(&into).expect("read the copy").is_empty(),
        "no invitation is copied"
    );
}
