//! **A venue-rotated cTrader grant lands in the change journal** — ONE record for the PAIR, with
//! the `venue` origin, and not a byte of either token.
//!
//! This call site is the reason `vike_change_journal::Actor::Venue` exists, and it is also
//! the one that was invisible everywhere: `crate::token_store` logs nothing (still true, apart from
//! a journal-append failure), so before this a daemon could rotate a live OAuth grant and leave no
//! trace on any surface in the workspace.
//!
//! ⚠ **The pair is ONE change, not two.** cTrader rotates the refresh token alongside the access
//! token — that is why `crate::token_store::persist` writes both in a single upsert, since a
//! half-write leaves a SPENT refresh token on disk. Two records would read as two rotations and
//! invite exactly that misreading back, so `count == 2` on one line is the gated shape.
//!
//! Everything is driven through the REAL `crate::config::CtraderConfig::from_credentials_with_store`
//! over the account's credentials as the store hands them back, so this also gates that the
//! composition root's ONE `state_dir` parameter produces BOTH the settings directory and the ledger
//! location, and that the rotation targets the account the grant was read from. A test that
//! hand-built a `TokenPersist` could not tell a wired derivation from two independent ones.

use std::path::Path;

use vike_change_journal::{CHANGES_SUBDIR, KIND_CREDENTIAL_WRITE, month_file_name};
use vike_ctrader::config::CtraderConfig;
use vike_ctrader::oauth::Token;
use vike_ctrader::token_store::persist;
use vike_model::AccountTier;
use vike_secrets::account_credentials::CredentialBook;

/// 2026-08-21T00:00:00Z — the injected instant. `persist` takes it as a parameter because
/// `vike_change_journal` reads no clock.
const T: i64 = 1_787_356_800_000;

/// The rotated pair. ⚠ Deliberately sharing no six-character run with anything a `credential_write`
/// line legitimately holds (`ctrader`, `vike.db`, `ACCESS_TOKEN`, the row names, the digits of
/// [`T`]) — otherwise [`assert_no_secret_window`] would be testing coincidence rather than leakage.
const FRESH_ACCESS: &str = "qzjvwx7413mfbphgnd8256wu";
const FRESH_REFRESH: &str = "gfpzmwqx9042hvbjntdu6531";

/// The shortest run of a token this test refuses to find in the ledger. Half a live grant is still a
/// live grant, so a whole-string `contains` check would be the wrong bound.
const MIN_LEAK_WINDOW: usize = 6;

/// Create a scratch settings directory's settings DATABASE (`vike-cli secrets init`'s library
/// half), the venue's application pair, and ONE ctrader account at `tier` holding a stale grant —
/// all through the record-less writer bodies, so the ledger holds the rotation's record alone.
/// Returns the account's id.
fn seed_store(settings: &Path, tier: AccountTier) -> i64 {
    vike_secrets::create_store(settings.to_str()).expect("create the empty store");
    let seed = |owner: &vike_secrets::CredentialOwner, fields: &[(&str, &str)]| {
        let rows: Vec<(String, String)> =
            fields.iter().map(|(k, v)| ((*k).to_string(), (*v).to_string())).collect();
        vike_secrets::save_owned_credentials_unjournalled(
            settings,
            owner,
            &rows,
            vike_secrets::UpsertIntent::Add,
        )
        .expect("seed the store");
    };
    seed(
        &vike_secrets::CredentialOwner::Venue("ctrader".to_string()),
        &[("CLIENT_ID", "app-id"), ("CLIENT_SECRET", "app-secret")],
    );
    let id = vike_secrets::edit_account_in_unjournalled(
        settings,
        vike_secrets::AccountEdit::Create { venue: "ctrader", tier, label: None },
    )
    .expect("create the account row")
    .after
    .expect("a created row")
    .id;
    seed(
        &vike_secrets::CredentialOwner::Account(id),
        &[("ACCESS_TOKEN", "AT_stale"), ("REFRESH_TOKEN", "RT_stale")],
    );
    id
}

/// Every account's and venue's fields, as a composition root reads them.
fn book(settings: &Path) -> CredentialBook {
    vike_secrets::CredentialStore::open(
        Some(vike_secrets::StoreLocation::new(settings)),
        vike_secrets::CredentialScope::Full,
    )
    .credential_book()
    .expect("the store reads")
}

fn token(access: &str, refresh: &str) -> Token {
    Token {
        access_token: access.to_string(),
        refresh_token: refresh.to_string(),
        expires_in: 2_592_000,
    }
}

fn assert_no_secret_window(raw: &str, secret: &str) {
    let chars: Vec<char> = secret.chars().collect();
    assert!(chars.len() >= MIN_LEAK_WINDOW, "the fixture is shorter than the window");
    for start in 0..=chars.len() - MIN_LEAK_WINDOW {
        for end in (start + MIN_LEAK_WINDOW)..=chars.len() {
            let window: String = chars[start..end].iter().collect();
            assert!(
                !raw.contains(&window),
                "the change journal carries a {}-character window of a rotated token ({window:?}). \
                 `Change::credential_write` takes no value parameter, so this means a token reached \
                 a cell that was supposed to hold a NAME.\nledger: {raw}",
                end - start
            );
        }
    }
}

/// THE gate: one rotation, one record, `origin: venue`, `count == 2`, no token anywhere in it.
#[test]
fn one_rotation_is_one_venue_record_naming_both_rows_and_neither_token() {
    let dir = tempfile::tempdir().unwrap();
    let settings = dir.path().join("settings");
    let state = settings.join("state");
    std::fs::create_dir_all(&state).unwrap();
    let id = seed_store(&settings, AccountTier::Demo);

    // The REAL config path: the account's credentials as the store hands them back, one
    // `state_dir` in, the settings directory, the account row and the ledger location out.
    let stored = book(&settings);
    let cfg = CtraderConfig::from_credentials_with_store(
        AccountTier::Demo,
        &stored.view("ctrader", Some(id)),
        Some(&state),
    )
    .expect("a complete demo grant gates live");
    let p = cfg.token_persist.expect("a declared state dir yields a persist target");
    assert_eq!(
        p.settings_dir, settings,
        "the store's directory is DERIVED from the same state dir"
    );
    assert_eq!(p.state_dir, state, "…and so is the ledger's home");
    assert_eq!(p.account_row, Some(id), "…and the rotation targets the account it was read from");

    persist(&p, &token(FRESH_ACCESS, FRESH_REFRESH), T).expect("persist");

    // Anti-vacuity FIRST: the rotation really landed. Without this, every assertion below would
    // stay green for a `persist` that had stopped writing the store entirely.
    let saved = book(&settings);
    let fields = saved.account(id).expect("the account's fields");
    assert_eq!(fields.get("ACCESS_TOKEN"), Some(FRESH_ACCESS));
    assert_eq!(fields.get("REFRESH_TOKEN"), Some(FRESH_REFRESH));
    assert_eq!(
        saved.venue("ctrader").and_then(|v| v.get("CLIENT_ID")),
        Some("app-id"),
        "other rows stay"
    );

    let ledger = state.join(CHANGES_SUBDIR).join(month_file_name(T));
    let raw = std::fs::read_to_string(&ledger).expect("the change journal was written");
    assert_eq!(raw.lines().count(), 1, "ONE rotation is ONE record, never one per field: {raw}");

    let v: serde_json::Value = serde_json::from_str(raw.trim_end()).expect("one JSON object");
    assert_eq!(v["kind"], KIND_CREDENTIAL_WRITE);
    assert_eq!(v["outcome"], "applied");
    // The VENUE rotated this, not a human at a keyboard — the whole reason that origin exists.
    assert_eq!(v["actor"]["origin"], "venue", "{raw}");
    assert_eq!(v["actor"]["venue"], "ctrader", "{raw}");
    assert!(v["actor"].get("user").is_none(), "no invented human actor: {raw}");

    assert_eq!(v["target"]["store"], vike_secrets::DB_FILE);
    assert_eq!(v["target"]["venue"], "ctrader");
    assert_eq!(v["target"]["tier"], "demo", "the tier is the account row's own: {raw}");
    assert_eq!(v["target"]["count"], 2, "the access token and the refresh token, together");
    let keys: Vec<&str> =
        v["target"]["keys"].as_array().unwrap().iter().map(|k| k.as_str().unwrap()).collect();
    assert_eq!(keys.len(), 2, "{raw}");
    assert!(keys[0].ends_with("ACCESS_TOKEN") && keys[1].ends_with("REFRESH_TOKEN"), "{raw}");

    assert_no_secret_window(&raw, FRESH_ACCESS);
    assert_no_secret_window(&raw, FRESH_REFRESH);
}

/// A LIVE account's record says `live`, so a reader can tell a demo rotation from a real one.
///
/// Cheap, and it is the assertion that stops the tier cell being pinned by a single fixture: a
/// hard-coded `"demo"` would pass the test above and be wrong about every live rotation.
#[test]
fn a_live_account_rotation_records_the_live_tier() {
    let dir = tempfile::tempdir().unwrap();
    let settings = dir.path().join("settings");
    let state = settings.join("state");
    std::fs::create_dir_all(&state).unwrap();
    let id = seed_store(&settings, AccountTier::Live);

    let stored = book(&settings);
    let cfg = CtraderConfig::from_credentials_with_store(
        AccountTier::Live,
        &stored.view("ctrader", Some(id)),
        Some(&state),
    )
    .expect("a complete live grant loads");
    let p = cfg.token_persist.expect("a persist target");
    persist(&p, &token(FRESH_ACCESS, FRESH_REFRESH), T).expect("persist");

    let raw = std::fs::read_to_string(state.join(CHANGES_SUBDIR).join(month_file_name(T))).unwrap();
    let v: serde_json::Value = serde_json::from_str(raw.trim_end()).unwrap();
    assert_eq!(v["target"]["tier"], "live", "{raw}");
    assert_eq!(v["target"]["count"], 2);
}

/// **No state directory ⇒ nothing is written and no path is invented** — not the store, not a
/// ledger.
///
/// A mount whose composition root resolved no project has nowhere legitimate to put either, and
/// `CtraderConfig::from_credentials` (the `state_dir`-less spelling every test and the catalog
/// probe use) is the shape that says so: no `token_persist`, so `crate::conn`'s
/// `adopt_refreshed_token` takes its early-return arm and the refresh lives as long as the process.
/// Asserted against a directory tree that stays EMPTY, so "wrote nothing" and "wrote somewhere else"
/// are distinguishable.
#[test]
fn a_stateless_mount_persists_nothing_and_invents_no_ledger() {
    use vike_secrets::account_credentials::{AccountCredentials, CredentialFields};

    let dir = tempfile::tempdir().unwrap();
    let before: Vec<_> = std::fs::read_dir(dir.path()).unwrap().flatten().collect();
    assert!(before.is_empty(), "precondition: the tree starts empty");

    let account =
        CredentialFields::from_pairs([("ACCESS_TOKEN", "AT_stale"), ("REFRESH_TOKEN", "RT_stale")]);
    let venue =
        CredentialFields::from_pairs([("CLIENT_ID", "app-id"), ("CLIENT_SECRET", "app-secret")]);
    let cfg = CtraderConfig::from_credentials(
        AccountTier::Demo,
        &AccountCredentials::new(Some(1), Some(&account), Some(&venue)),
    )
    .expect("the grant still gates live — only the rotation home is absent");
    assert!(
        cfg.token_persist.is_none(),
        "no state dir means no store to rotate into, and therefore no ledger to record into"
    );

    // …and nothing appeared anywhere: not a `changes/`, not a `state/`, not a database.
    let after: Vec<_> = std::fs::read_dir(dir.path()).unwrap().flatten().collect();
    assert!(after.is_empty(), "a stateless mount wrote something: {after:?}");
    assert!(!Path::new(&dir.path().join(CHANGES_SUBDIR)).exists());
}
