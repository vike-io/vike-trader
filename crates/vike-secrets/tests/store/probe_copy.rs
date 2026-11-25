//! **A deploy pre-flight's probe copy holds no secret and every other row** (`vike_secrets::write_probe_copy`, what
//! `deploy/docker/vikectl`'s `probe_copy` runs through `vike-cli backend probe-copy` instead of the `sqlite3` tool).
//!
//! Over a real store holding a credential, a hub key, an account with a note, an invitation, a device's private key
//! and a SECRET venue field (Polymarket's proxy URL with a password, and a note; stored UPPER-case by the production
//! writer, and once more lower-case) beside a plain one: the copy's BYTES
//! hold none of those values, each value is its placeholder, the plain venue field is as it was, the invitations'
//! table is gone, the device's row stays with the caller's THROWAWAY key in place of its own, the per-daemon rules
//! apply (a key dropped by name, every account `paper`, a numeric placeholder), a settings row and the schema version
//! survive, the file is 0600, the live store is untouched, and a second copy over the first is refused. A throwaway
//! that IS the live key is refused by the read-back, and leaves no file.

use std::path::Path;

use vike_secrets::{
    AccountEdit, CredentialUpsertIntent, ProbeCopy, SCHEMA_VERSION, StoreTable, write_probe_copy,
};
use vike_venues::venue_fields::{SOCKS_PROXY, VenueFieldName, WS_PROXY_ENABLED};

const SECRET: &str = "credential-secret-value-1f2e";
const HUB_SECRET: &str = "hub-key-secret-value-9a8b";
const BUILDER: &str = "builder-secret-value-77c1";
const NOTE: &str = "a private account note 3d4c";
const DEVICE_SECRET: &str = "device-private-key-5e6f";
const INVITE_SECRET: &str = "invite-secret-0a0b";
const PROXY_SECRET: &str = "socks5://probe-user:proxy-password-6a7b@<host>:1080";
const PROXY_LOWER: &str = "socks5://probe-user:proxy-password-1e2f@<host>:1080";
const PROXY_NOTE: &str = "the old proxy password was 2c3d";
/// The secret venue fields the caller names (`vike_venues::venue_fields`'s `secret` rows).
const SECRET_FIELDS: &[(&str, VenueFieldName)] = &[("polymarket", SOCKS_PROXY)];
/// What the test's `throwaway` hands in for the device row: a key id and a document (no crypto here).
const THROWAWAY_ID: &str = "nk-throwaway";
const THROWAWAY_KEY: &str = "throwaway-device-key-8c9d";

fn rows(db: &Path, sql: &str) -> Vec<String> {
    let conn = rusqlite::Connection::open(db).expect("open");
    let mut stmt = conn.prepare(sql).expect("prepare");
    stmt.query_map([], |r| r.get::<_, String>(0)).expect("query").map(Result::unwrap).collect()
}

#[test]
fn a_probe_copy_holds_no_secret_and_every_other_row() {
    let dir = tempfile::tempdir().expect("tempdir");
    let live = dir.path().join("live/settings");
    vike_secrets::plant_settings_rows(
        &live,
        &vike_secrets::StoredSettings {
            settings: vec![vike_secrets::SettingRow {
                section: "config".to_string(),
                key: "tradehub_addr".to_string(),
                value: "\"127.0.0.1:17879\"".to_string(),
            }],
            ..Default::default()
        },
    )
    .expect("a store with one settings row");
    let pairs = |p: &[(&str, &str)]| -> Vec<(String, String)> {
        p.iter().map(|(n, v)| ((*n).to_string(), (*v).to_string())).collect()
    };
    vike_secrets::save_credentials_to_store_unjournalled(
        &live,
        StoreTable::Credential,
        &pairs(&[
            ("SOME_SECRET", SECRET),
            ("VIKE_STRATEGY_BUILDER_KEY", BUILDER),
            ("VIKE_TELEGRAM_ALLOWED_CHAT_IDS", "123456"),
        ]),
        CredentialUpsertIntent::Add,
    )
    .expect("credentials");
    vike_secrets::save_credentials_to_store_unjournalled(
        &live,
        StoreTable::HubKey,
        &pairs(&[("SOME_HUB_KEY", HUB_SECRET)]),
        CredentialUpsertIntent::Add,
    )
    .expect("a hub key");
    vike_secrets::edit_account_in_unjournalled(
        &live,
        AccountEdit::Create { venue: "bybit", tier: vike_model::AccountTier::Demo, label: None },
    )
    .expect("an account");
    vike_secrets::access::add_access_invite(
        &live,
        "0a1b2c3d",
        "alice",
        "observe",
        INVITE_SECRET.as_bytes(),
        i64::MAX,
    )
    .expect("an invitation");
    // The venue fields as the production writer stores them (`vike-cli config set` upper-cases the field), and one
    // secret row spelled lower-case, as the vocabulary spells it: the copy must find both.
    let stamp = || vike_secrets::ChangeStamp {
        actor: vike_change_journal::ChangeActor::cli("test"),
        process: vike_change_journal::ChangeProcess::new("test", 1, "0.0.0"),
        now_ms: 1,
    };
    for (field, value, secret) in
        [(SOCKS_PROXY, PROXY_SECRET, true), (WS_PROXY_ENABLED, "true", false)]
    {
        vike_secrets::set_venue_setting_in(
            &live,
            "polymarket",
            None,
            &field,
            value,
            secret,
            stamp(),
        )
        .expect("a venue field, as the production writer stores it");
    }
    vike_secrets::plant_venue_setting_for_test(
        &live,
        "polymarket",
        None,
        "socks_proxy",
        PROXY_LOWER,
    )
    .expect("a secret venue field spelled lower-case, as hand-typed SQL may store it");
    let db = vike_secrets::db_path_in(&live);
    assert_eq!(
        rows(&db, "SELECT field FROM venue_setting ORDER BY field"),
        vec!["SOCKS_PROXY", "WS_PROXY_ENABLED", "socks_proxy"],
        "the production writer stores a field UPPER-case: the case the copy must match"
    );
    {
        let conn = rusqlite::Connection::open(&db).expect("open the live store");
        // `add_access_invite` created the access tables (decision 0288's DDL); the device's own private key row.
        conn.execute_batch(&format!(
            "UPDATE account SET notes = '{NOTE}'; \
             UPDATE venue_setting SET notes = '{PROXY_NOTE}' WHERE upper(field) = 'SOCKS_PROXY'; \
             INSERT INTO device_key (key_id, person, device, private_key, created_ms) \
             VALUES ('k1', 'owner', 'pc', CAST('{DEVICE_SECRET}' AS BLOB), 0);"
        ))
        .expect("plant a note, a proxy note and a device key");
    }
    let before = std::fs::read(&db).expect("the live store's bytes");

    let into = dir.path().join("probe/settings");
    let dropped = vec!["VIKE_STRATEGY_BUILDER_KEY".to_string()];
    let numeric = vec!["VIKE_TELEGRAM_ALLOWED_CHAT_IDS".to_string()];
    let rules = ProbeCopy {
        drop_keys: &dropped,
        paper_accounts: true,
        numeric_keys: &numeric,
        secret_venue_fields: SECRET_FIELDS,
    };
    let mut throwaway =
        || Ok::<_, std::io::Error>((THROWAWAY_ID.to_string(), THROWAWAY_KEY.as_bytes().to_vec()));
    write_probe_copy(&db, &into, &rules, &mut throwaway).expect("the probe copy");

    let copy = vike_secrets::db_path_in(&into);
    let bytes =
        String::from_utf8_lossy(&std::fs::read(&copy).expect("the copy's bytes")).to_string();
    for secret in [
        SECRET,
        HUB_SECRET,
        BUILDER,
        NOTE,
        DEVICE_SECRET,
        INVITE_SECRET,
        PROXY_SECRET,
        PROXY_LOWER,
        PROXY_NOTE,
        "proxy-password-6a7b",
        "proxy-password-1e2f",
    ] {
        assert!(!bytes.contains(secret), "the copy's bytes hold `{secret}`");
    }
    assert_eq!(
        rows(
            &copy,
            "SELECT field FROM venue_setting \
             WHERE upper(field) = 'SOCKS_PROXY' AND value = 'probe-' || id AND notes IS NULL \
             ORDER BY field"
        ),
        vec!["SOCKS_PROXY", "socks_proxy"],
        "each secret venue row, upper- and lower-case, holds its placeholder and no note"
    );
    assert_eq!(
        rows(
            &copy,
            "SELECT field || '=' || value || ' ' || coalesce(notes, '-') FROM venue_setting \
             WHERE upper(field) <> 'SOCKS_PROXY'"
        ),
        vec!["WS_PROXY_ENABLED=true -"],
        "a plain venue field is as it was"
    );
    assert_eq!(
        rows(&copy, "SELECT name || '=' || value FROM credential WHERE name = 'SOME_SECRET'"),
        rows(&copy, "SELECT name || '=probe-' || id FROM credential WHERE name = 'SOME_SECRET'"),
        "a credential's value is its placeholder"
    );
    assert!(
        rows(&copy, "SELECT name FROM credential WHERE name = 'VIKE_STRATEGY_BUILDER_KEY'")
            .is_empty()
    );
    assert_eq!(
        rows(&copy, "SELECT value FROM credential WHERE name = 'VIKE_TELEGRAM_ALLOWED_CHAT_IDS'"),
        rows(
            &copy,
            "SELECT CAST(id AS TEXT) FROM credential WHERE name = 'VIKE_TELEGRAM_ALLOWED_CHAT_IDS'"
        ),
        "a numeric key's placeholder is its row id"
    );
    assert_eq!(rows(&copy, "SELECT tier FROM account"), vec!["paper".to_string()]);
    assert!(rows(&copy, "SELECT notes FROM account WHERE notes IS NOT NULL").is_empty());
    assert!(
        rows(&copy, "SELECT name FROM sqlite_master WHERE name = 'access_invite'").is_empty(),
        "the invitations' table is dropped whole"
    );
    assert_eq!(
        rows(
            &copy,
            "SELECT key_id || ' ' || person || '/' || device || ' ' || CAST(private_key AS TEXT) \
             FROM device_key"
        ),
        vec![format!("{THROWAWAY_ID} owner/pc {THROWAWAY_KEY}")],
        "the device's row stays, under the throwaway key and its id"
    );
    assert_eq!(
        rows(&copy, "SELECT section || '.' || key || '=' || value FROM setting"),
        vec!["config.tradehub_addr=\"127.0.0.1:17879\"".to_string()],
        "a settings row is copied as it is"
    );
    let conn = rusqlite::Connection::open(&copy).expect("open the copy");
    let version: i64 = conn.query_row("PRAGMA user_version", [], |r| r.get(0)).expect("version");
    assert_eq!(version, SCHEMA_VERSION, "the copy is a store this build reads");
    drop(conn);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(&copy).expect("metadata").permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "the copy is the owner's alone");
    }

    assert_eq!(
        std::fs::read(&db).expect("the live store's bytes"),
        before,
        "the live store is untouched"
    );
    assert!(
        write_probe_copy(&db, &into, &ProbeCopy::default(), &mut throwaway).is_err(),
        "a copy is never written over an existing one"
    );

    // The read-back's floor: a "throwaway" that is the live key is written, found and refused, and the file goes.
    let leaked = dir.path().join("leaked/settings");
    let mut the_live_key =
        || Ok::<_, std::io::Error>((THROWAWAY_ID.to_string(), DEVICE_SECRET.as_bytes().to_vec()));
    let refused = write_probe_copy(&db, &leaked, &ProbeCopy::default(), &mut the_live_key);
    assert!(refused.is_err(), "a copy holding the live private key is refused");
    assert!(!vike_secrets::db_path_in(&leaked).exists(), "…and removed");
}
