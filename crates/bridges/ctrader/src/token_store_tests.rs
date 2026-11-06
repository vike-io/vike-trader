use super::*;

use vike_model::AccountTier;
use vike_secrets::account_credentials::CredentialBook;

fn token(access: &str, refresh: &str, expires_in: u64) -> crate::oauth::Token {
    crate::oauth::Token {
        access_token: access.to_string(),
        refresh_token: refresh.to_string(),
        expires_in,
    }
}

/// A persist target for the settings directory `dir` and account row `account_row`, with the
/// state directory inside it exactly as `CtraderConfig::from_credentials_with_store` derives the
/// pair.
fn persist_for(dir: &Path, account_row: Option<i64>) -> TokenPersist {
    TokenPersist { settings_dir: dir.to_path_buf(), account_row, state_dir: dir.join("state") }
}

/// 2026-08-21T00:00:00Z — the injected instant every rotation below is stamped with.
const T: i64 = 1_787_356_800_000;

/// A settings DATABASE under `dir`, created the way a fresh box gets one (`vike-cli secrets init`'s
/// library half), holding cTrader's application pair as the VENUE's fields — seeded through the
/// record-less writer bodies (a fixture, not a change worth a journal line).
fn store_with_app_pair(dir: &Path) {
    vike_secrets::create_store(dir.to_str()).expect("create the empty store");
    seed(
        dir,
        &vike_secrets::CredentialOwner::Venue("ctrader".to_string()),
        &[("CLIENT_ID", "app-id"), ("CLIENT_SECRET", "app-secret")],
    );
}

/// A ctrader `account` row at `tier` (labelled `label`), holding `fields` — the operator's act
/// (`vike-cli secrets account add`) and then the account's keys. Returns the row's id.
fn account(dir: &Path, tier: AccountTier, label: Option<&str>, fields: &[(&str, &str)]) -> i64 {
    let id = vike_secrets::edit_account_in_unjournalled(
        dir,
        vike_secrets::AccountEdit::Create { venue: "ctrader", tier, label },
    )
    .expect("create the account row")
    .after
    .expect("a created row")
    .id;
    seed(dir, &vike_secrets::CredentialOwner::Account(id), fields);
    id
}

fn seed(dir: &Path, owner: &vike_secrets::CredentialOwner, fields: &[(&str, &str)]) {
    let rows: Vec<(String, String)> =
        fields.iter().map(|(k, v)| ((*k).to_string(), (*v).to_string())).collect();
    vike_secrets::save_owned_credentials_unjournalled(
        dir,
        owner,
        &rows,
        vike_secrets::UpsertIntent::Add,
    )
    .expect("seed the store");
}

fn handle(dir: &Path) -> vike_secrets::CredentialStore {
    vike_secrets::CredentialStore::open(
        Some(vike_secrets::StoreLocation::new(dir)),
        vike_secrets::CredentialScope::Full,
    )
}

/// What the store holds — every credential row by NAME (the whole table), read through the handle
/// every reader uses.
fn store_rows(dir: &Path) -> std::collections::HashMap<String, String> {
    handle(dir).read_all().expect("the store reads").secrets.into_map()
}

/// Every account's and venue's fields, by FIELD.
fn book(dir: &Path) -> CredentialBook {
    handle(dir).credential_book().expect("the store reads")
}

/// The settings directory is the parent of the state directory the root declared — a derivation,
/// never a walk — and the store is the database inside it.
#[test]
fn the_settings_dir_is_derived_from_the_declared_state_dir() {
    let settings = Path::new("/srv/vike-<unit>/settings");
    assert_eq!(
        settings_dir_beside_state_dir(&settings.join("state")),
        Some(settings.to_path_buf())
    );
    assert_eq!(persist_for(settings, Some(1)).store(), vike_secrets::db_path_in(settings));
}

/// THE property the whole change rests on: a refreshed pair lands on its account's two FIELDS and
/// every other credential is untouched — the account's own `ACCOUNT_ID`, another account's grant,
/// and the venue's application pair. Asserted through the REAL store write so the transaction is
/// covered too.
#[test]
fn a_refresh_rewrites_only_its_two_fields_and_preserves_everything_else() {
    let dir = tempfile::tempdir().unwrap();
    store_with_app_pair(dir.path());
    let demo = account(
        dir.path(),
        AccountTier::Demo,
        None,
        &[("ACCESS_TOKEN", "AT_stale"), ("REFRESH_TOKEN", "RT_stale"), ("ACCOUNT_ID", "42")],
    );
    let live = account(
        dir.path(),
        AccountTier::Live,
        None,
        &[("ACCESS_TOKEN", "AT_live_untouched"), ("REFRESH_TOKEN", "RT_live_untouched")],
    );
    let before = store_rows(dir.path());
    assert_eq!(before.len(), 7, "the fixture seeded every row");

    persist(&persist_for(dir.path(), Some(demo)), &token("AT_fresh", "RT_fresh", 2_592_000), T)
        .unwrap();

    let after = store_rows(dir.path());
    assert_eq!(before.len(), after.len(), "no credential may be added or dropped");
    let book = book(dir.path());
    let mine = book.account(demo).expect("the rotated account");
    assert_eq!(mine.get("ACCESS_TOKEN"), Some("AT_fresh"));
    assert_eq!(mine.get("REFRESH_TOKEN"), Some("RT_fresh"));
    assert_eq!(mine.get("ACCOUNT_ID"), Some("42"), "the account's other field is untouched");
    let other = book.account(live).expect("the other account");
    assert_eq!(other.get("ACCESS_TOKEN"), Some("AT_live_untouched"));
    assert_eq!(other.get("REFRESH_TOKEN"), Some("RT_live_untouched"));
    let app = book.venue("ctrader").expect("the venue's application pair");
    assert_eq!(
        (app.get("CLIENT_ID"), app.get("CLIENT_SECRET")),
        (Some("app-id"), Some("app-secret"))
    );
}

/// A persisted grant is picked up by a FRESH loader — the end-to-end proof that the write half
/// and the read half agree, driven through the real `from_credentials` over the account's view.
#[test]
fn a_persisted_grant_is_picked_up_by_a_fresh_loader() {
    let dir = tempfile::tempdir().unwrap();
    store_with_app_pair(dir.path());
    let id = account(
        dir.path(),
        AccountTier::Demo,
        None,
        &[("ACCESS_TOKEN", "AT_stale"), ("REFRESH_TOKEN", "RT_stale")],
    );

    persist(&persist_for(dir.path(), Some(id)), &token("AT_rotated", "RT_rotated", 100), T)
        .unwrap();

    // A cold start: re-read the store exactly as a composition root would, then load the venue
    // config from the account's credentials.
    let book = book(dir.path());
    let cfg = crate::config::CtraderConfig::from_credentials(
        AccountTier::Demo,
        &book.view("ctrader", Some(id)),
    )
    .expect("the rotated grant gates live");
    assert_eq!(cfg.access_token, "AT_rotated");
    assert_eq!(cfg.refresh_token, "RT_rotated");
}

/// A rotation with NO store is an ERROR that names the path and the account, and never the token —
/// a grant the venue has already exchanged must not be "saved" somewhere nothing reads.
#[test]
fn a_persist_failure_names_the_path_and_the_account_but_never_the_token() {
    let dir = tempfile::tempdir().unwrap();
    // No settings database: there is nowhere to write.
    let p = persist_for(dir.path(), Some(7));
    let err = persist(&p, &token("AT_SECRET_LEAK", "RT_SECRET_LEAK", 10), T)
        .expect_err("with no settings database there is no store to write");
    let msg = err.to_string();
    assert!(msg.contains(&p.store().display().to_string()), "{msg}");
    assert!(msg.contains("account row 7"), "{msg}");
    assert!(msg.contains("ACCESS_TOKEN"), "{msg}");
    assert!(msg.contains("secrets init"), "the refusal must name the way in: {msg}");
    assert!(!msg.contains("AT_SECRET_LEAK"), "leaked the access token: {msg}");
    assert!(!msg.contains("RT_SECRET_LEAK"), "leaked the refresh token: {msg}");
    assert!(
        !vike_secrets::db_path_in(dir.path()).exists(),
        "a refused rotation must not bring a store into being"
    );
}

/// **Credentials that name no account row cannot be rotated back** — refused loudly, the store
/// untouched, and the refusal says why without a token in it.
#[test]
fn a_rotation_that_names_no_account_is_refused_and_writes_nothing() {
    let dir = tempfile::tempdir().unwrap();
    store_with_app_pair(dir.path());
    let id = account(
        dir.path(),
        AccountTier::Demo,
        None,
        &[("ACCESS_TOKEN", "AT_stale"), ("REFRESH_TOKEN", "RT_stale")],
    );
    let before = store_rows(dir.path());

    let err = persist(&persist_for(dir.path(), None), &token("AT_SECRET_LEAK", "RT_x", 10), T)
        .expect_err("no account row, no write");
    let msg = err.to_string();
    assert!(msg.contains("no account row"), "{msg}");
    assert!(!msg.contains("AT_SECRET_LEAK"), "leaked the access token: {msg}");
    assert_eq!(store_rows(dir.path()), before, "a refused rotation writes nothing");
    assert_eq!(book(dir.path()).account(id).and_then(|f| f.get("ACCESS_TOKEN")), Some("AT_stale"));
}

/// An account id no row carries is refused by the writer (never a create), and a field the account
/// does not hold is refused too (`Replace`: a rotation replaces the two rows the grant was READ
/// from and never invents one). Either way the store is untouched.
#[test]
fn a_rotation_never_creates_an_account_or_a_field() {
    let dir = tempfile::tempdir().unwrap();
    store_with_app_pair(dir.path());
    // Only half a grant: the REFRESH_TOKEN row does not exist.
    let id = account(dir.path(), AccountTier::Demo, None, &[("ACCESS_TOKEN", "AT_stale")]);
    let before = store_rows(dir.path());

    persist(&persist_for(dir.path(), Some(id + 100)), &token("AT_new", "RT_new", 10), T)
        .expect_err("an account no row carries is refused");
    assert_eq!(store_rows(dir.path()), before, "no account was created and nothing was written");

    persist(&persist_for(dir.path(), Some(id)), &token("AT_new", "RT_new", 10), T)
        .expect_err("a field the account does not hold is refused, never created");
    assert_eq!(store_rows(dir.path()), before, "the pair is one transaction: nothing half-landed");
}

/// The expiry arithmetic and the margin, exactly — driven by an injected `now_ms`, never a wall
/// clock.
#[test]
fn refresh_is_due_only_inside_the_margin() {
    let at = expires_at_ms(1_000_000, 2_592_000).expect("a real lifetime has an expiry");
    assert_eq!(at, 1_000_000 + 2_592_000 * 1000);

    assert!(!refresh_due(Some(at), at - REFRESH_MARGIN_MS - 1));
    assert!(refresh_due(Some(at), at - REFRESH_MARGIN_MS));
    assert!(refresh_due(Some(at), at + 1), "an ALREADY-lapsed grant is due");
    // An unknown expiry is never due — it must not refresh on every heartbeat forever.
    assert!(!refresh_due(None, i64::MAX));
    // …and a venue that reported no lifetime yields no opinion rather than an instant refresh.
    assert_eq!(expires_at_ms(5, 0), None);
}

/// **A rotation on one account must target that account's own fields.**
///
/// This is the one way this venue could DAMAGE a stored credential rather than merely mis-read
/// one: [`persist`] upserts whatever account it is handed, so a rotation filed under the wrong
/// owner would make every routine refresh on a second account overwrite the first account's grant
/// in the shared credential store — breaking the account that was working, silently, on a timer.
#[test]
fn a_rotation_targets_its_own_account_and_no_other() {
    let dir = tempfile::tempdir().unwrap();
    store_with_app_pair(dir.path());
    let default = account(
        dir.path(),
        AccountTier::Demo,
        None,
        &[("ACCESS_TOKEN", "AT_default"), ("REFRESH_TOKEN", "RT_default")],
    );
    let alt = account(
        dir.path(),
        AccountTier::Demo,
        Some("ALT"),
        &[("ACCESS_TOKEN", "AT_alt"), ("REFRESH_TOKEN", "RT_alt")],
    );

    persist(&persist_for(dir.path(), Some(alt)), &token("AT_alt_new", "RT_alt_new", 10), T)
        .unwrap();

    let book = book(dir.path());
    let alt_fields = book.account(alt).expect("ALT");
    assert_eq!(alt_fields.get("ACCESS_TOKEN"), Some("AT_alt_new"));
    assert_eq!(alt_fields.get("REFRESH_TOKEN"), Some("RT_alt_new"));
    let default_fields = book.account(default).expect("the default account");
    assert_eq!(
        (default_fields.get("ACCESS_TOKEN"), default_fields.get("REFRESH_TOKEN")),
        (Some("AT_default"), Some("RT_default")),
        "a rotation on ALT must not write the default account's grant"
    );
}
