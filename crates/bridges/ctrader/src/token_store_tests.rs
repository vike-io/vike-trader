use super::*;

fn token(access: &str, refresh: &str, expires_in: u64) -> crate::oauth::Token {
    crate::oauth::Token {
        access_token: access.to_string(),
        refresh_token: refresh.to_string(),
        expires_in,
    }
}

/// A DEMO-tier persist target under `dir`, with the state directory beside the store exactly as
/// `CtraderConfig::from_vars_with_store` derives the pair.
fn demo_persist(dir: &Path, store: PathBuf) -> TokenPersist {
    TokenPersist {
        store,
        keys: TokenKeys::for_env(Environment::Demo),
        state_dir: dir.join("state"),
    }
}

/// 2026-08-21T00:00:00Z — the injected instant every rotation below is stamped with.
const T: i64 = 1_787_356_800_000;

/// The key names are the ones the LOADER reads — pinned per tier, because a rotation that wrote
/// a key `from_vars` does not read would silently persist nothing.
#[test]
fn the_persisted_keys_are_the_keys_the_loader_reads() {
    let demo = TokenKeys::for_env(Environment::Demo);
    assert_eq!(demo.access, "CTRADER_DEMO_ACCESS_TOKEN");
    assert_eq!(demo.refresh, "CTRADER_DEMO_REFRESH_TOKEN");
    let live = TokenKeys::for_env(Environment::Live);
    assert_eq!(live.access, "CTRADER_LIVE_ACCESS_TOKEN");
    assert_eq!(live.refresh, "CTRADER_LIVE_REFRESH_TOKEN");
}

/// The store sits beside the state directory the root declared — a derivation, never a walk.
#[test]
fn the_store_is_derived_from_the_declared_state_dir() {
    let state = Path::new("/srv/vike-<unit>/settings/state");
    assert_eq!(
        store_path_beside_state_dir(state),
        Some(PathBuf::from("/srv/vike-<unit>/settings").join(vike_secrets::SECRETS_FILE))
    );
}

/// Carry `text` into a fresh settings DATABASE under `dir` — the one way a store comes into being
/// (`vike-cli secrets migrate`'s library half). The file is left exactly as written.
fn migrated_store(dir: &Path, text: &str) -> PathBuf {
    let store = dir.join(vike_secrets::SECRETS_FILE);
    std::fs::write(&store, text).unwrap();
    vike_secrets::migrate(
        dir.to_str(),
        vike_model::credential_keys::is_platform_key,
        &vike_bridge_core::credentials::classify_credential_name,
        vike_secrets::WhenNothingToCarry::CreateNothing,
    )
    .expect("the carry creates the store");
    store
}

/// What the store holds — its credential rows, read through the database the way every reader
/// sees it (`vike_secrets::read_table`).
fn store_rows(dir: &Path) -> std::collections::HashMap<String, String> {
    vike_secrets::read_table(&vike_secrets::db_path_in(dir), vike_secrets::Table::Credential)
        .expect("the store reads")
        .into_map()
}

/// THE property the whole change rests on: a refreshed pair lands on its two ROWS and every other
/// credential is untouched — and the credential FILE beside the database, which nothing reads, is
/// byte-identical. Asserted through the REAL store write so the transaction is covered too.
#[test]
fn a_refresh_rewrites_only_its_two_keys_and_preserves_everything_else() {
    let dir = tempfile::tempdir().unwrap();
    let original = "\
# vike credential store — hand-edited, keep the comments
BINANCE_LIVE_API_KEY=binance-key
BINANCE_LIVE_API_SECRET=binance-secret

# cTrader (the OAuth pair rotates on refresh)
CTRADER_CLIENT_ID=app-id
CTRADER_CLIENT_SECRET=app-secret
CTRADER_DEMO_ACCESS_TOKEN=AT_stale
CTRADER_DEMO_REFRESH_TOKEN=RT_stale
CTRADER_DEMO_ACCOUNT_ID=12345

OKX_DEMO_API_PASSPHRASE=okx-pass
";
    let store = migrated_store(dir.path(), original);
    let before = store_rows(dir.path());

    let p = demo_persist(dir.path(), store.clone());
    persist(&p, &token("AT_fresh", "RT_fresh", 2_592_000), T).unwrap();

    let after = store_rows(dir.path());
    assert_eq!(before.len(), after.len(), "no credential may be added or dropped");
    for (name, value) in &before {
        if name == "CTRADER_DEMO_ACCESS_TOKEN" || name == "CTRADER_DEMO_REFRESH_TOKEN" {
            continue;
        }
        assert_eq!(after.get(name), Some(value), "{name} must be untouched");
    }
    assert_eq!(after.get("CTRADER_DEMO_ACCESS_TOKEN").map(String::as_str), Some("AT_fresh"));
    assert_eq!(after.get("CTRADER_DEMO_REFRESH_TOKEN").map(String::as_str), Some("RT_fresh"));
    assert_eq!(
        std::fs::read_to_string(&store).unwrap(),
        original,
        "the credential FILE is not the store and must not be written"
    );
}

/// A persisted grant is picked up by a FRESH loader — the end-to-end proof that the write half
/// and the read half agree, driven through the real `from_vars`.
#[test]
fn a_persisted_grant_is_picked_up_by_a_fresh_loader() {
    let dir = tempfile::tempdir().unwrap();
    let store = migrated_store(
        dir.path(),
        "CTRADER_CLIENT_ID=app\nCTRADER_CLIENT_SECRET=sec\n\
             CTRADER_DEMO_ACCESS_TOKEN=AT_stale\nCTRADER_DEMO_REFRESH_TOKEN=RT_stale\n",
    );

    let p = demo_persist(dir.path(), store);
    persist(&p, &token("AT_rotated", "RT_rotated", 100), T).unwrap();

    // A cold start: re-read the store exactly as a composition root would, then load the venue
    // config from it.
    let vars = store_rows(dir.path());
    let cfg = crate::config::CtraderConfig::from_vars(Environment::Demo, &vars)
        .expect("the rotated grant gates live");
    assert_eq!(cfg.access_token, "AT_rotated");
    assert_eq!(cfg.refresh_token, "RT_rotated");
}

/// A rotation with NO store is an ERROR that names the path and the keys, and never the token —
/// a grant the venue has already exchanged must not be "saved" into a file nothing reads.
#[test]
fn a_persist_failure_names_the_path_and_keys_but_never_the_token() {
    let dir = tempfile::tempdir().unwrap();
    // A credential FILE and no database: the file store is gone, so there is nowhere to write.
    let store = dir.path().join(vike_secrets::SECRETS_FILE);
    std::fs::write(&store, "CTRADER_DEMO_ACCESS_TOKEN=AT_stale\n").unwrap();

    let p = demo_persist(dir.path(), store.clone());
    let err = persist(&p, &token("AT_SECRET_LEAK", "RT_SECRET_LEAK", 10), T)
        .expect_err("with no settings database there is no store to write");
    let msg = err.to_string();
    assert!(msg.contains(&store.display().to_string()), "{msg}");
    assert!(msg.contains("CTRADER_DEMO_ACCESS_TOKEN"), "{msg}");
    assert!(msg.contains("migrate"), "the refusal must name the way in: {msg}");
    assert!(!msg.contains("AT_SECRET_LEAK"), "leaked the access token: {msg}");
    assert!(!msg.contains("RT_SECRET_LEAK"), "leaked the refresh token: {msg}");
    assert_eq!(
        std::fs::read_to_string(&store).unwrap(),
        "CTRADER_DEMO_ACCESS_TOKEN=AT_stale\n",
        "the credential FILE must not be written"
    );
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

/// **A rotation on a LABELLED account must target that account's own keys.**
///
/// This is the one way this venue could DAMAGE a stored credential rather than merely mis-read
/// one: [`persist`] upserts whatever [`TokenKeys`] names, so a `for_env` here would make every
/// routine refresh on a second account overwrite the DEFAULT account's grant in the shared
/// credential store — breaking the account that was working, silently, on a timer.
///
/// The default account's two names are asserted as an EQUALITY against [`TokenKeys::for_env`]
/// rather than as literals, so this is byte-identity rather than a second copy of the spelling.
#[test]
fn a_labelled_accounts_rotation_targets_its_own_keys_and_the_default_accounts_do_not_move() {
    let alt = AccountLabel::parse("ALT").expect("a legal label");
    for env in [Environment::Demo, Environment::Live, Environment::Sim] {
        let default = TokenKeys::for_account(env, &AccountLabel::Default);
        assert_eq!(default, TokenKeys::for_env(env), "the default account's keys must not move");

        let labelled = TokenKeys::for_account(env, &alt);
        assert_eq!(labelled.access, format!("{}__ALT", default.access));
        assert_eq!(labelled.refresh, format!("{}__ALT", default.refresh));
        assert_ne!(
            labelled.access, default.access,
            "a labelled rotation must not write the default account's access token"
        );
        assert_ne!(labelled.refresh, default.refresh);
    }
}
