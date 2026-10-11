use super::*;

fn token(access: &str, refresh: &str, expires_in: u64) -> crate::oauth::Token {
    crate::oauth::Token {
        access_token: access.to_string(),
        refresh_token: refresh.to_string(),
        expires_in,
    }
}

/// A DEMO-tier persist target for the settings directory `dir`, with the state directory inside it
/// exactly as `CtraderConfig::from_vars_with_store` derives the pair.
fn demo_persist(dir: &Path) -> TokenPersist {
    TokenPersist {
        settings_dir: dir.to_path_buf(),
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

/// The settings directory is the parent of the state directory the root declared — a derivation,
/// never a walk — and the store is the database inside it.
#[test]
fn the_settings_dir_is_derived_from_the_declared_state_dir() {
    let settings = Path::new("/srv/vike-<unit>/settings");
    assert_eq!(
        settings_dir_beside_state_dir(&settings.join("state")),
        Some(settings.to_path_buf())
    );
    assert_eq!(demo_persist(settings).store(), vike_secrets::db_path_in(settings));
}

/// A settings DATABASE under `dir` holding `rows` — created the way a fresh box gets one
/// (`vike-cli secrets init`'s library half) and seeded through the one sanctioned writer.
fn seeded_store(dir: &Path, rows: &[(&str, &str)]) {
    vike_secrets::create_store(dir.to_str()).expect("create the empty store");
    let rows: Vec<(String, String)> =
        rows.iter().map(|(k, v)| ((*k).to_string(), (*v).to_string())).collect();
    vike_secrets::save_credentials_to_store(
        dir,
        vike_secrets::Table::Credential,
        &rows,
        Some(&vike_bridge_core::credentials::classify_credential_name),
    )
    .expect("seed the store");
}

/// What the store holds — its credential rows, read through the database the way every reader
/// sees it (`vike_secrets::read_table`).
fn store_rows(dir: &Path) -> std::collections::HashMap<String, String> {
    vike_secrets::read_table(&vike_secrets::db_path_in(dir), vike_secrets::Table::Credential)
        .expect("the store reads")
        .into_map()
}

/// THE property the whole change rests on: a refreshed pair lands on its two ROWS and every other
/// credential is untouched. Asserted through the REAL store write so the transaction is covered too.
#[test]
fn a_refresh_rewrites_only_its_two_keys_and_preserves_everything_else() {
    let dir = tempfile::tempdir().unwrap();
    // The neighbours are the app pair and the OTHER tier's grant — the rows a rotation sits closest
    // to. (Names outside this venue would be env-shaped literals in a `src/` file, which the
    // settings registry reads as undeclared reads by this crate.)
    seeded_store(
        dir.path(),
        &[
            ("CTRADER_CLIENT_ID", "app-id"),
            ("CTRADER_CLIENT_SECRET", "app-secret"),
            ("CTRADER_DEMO_ACCESS_TOKEN", "AT_stale"),
            ("CTRADER_DEMO_REFRESH_TOKEN", "RT_stale"),
            ("CTRADER_LIVE_ACCESS_TOKEN", "AT_live_untouched"),
            ("CTRADER_LIVE_REFRESH_TOKEN", "RT_live_untouched"),
        ],
    );
    let before = store_rows(dir.path());
    assert_eq!(before.len(), 6, "the fixture seeded every row");

    let p = demo_persist(dir.path());
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
}

/// A persisted grant is picked up by a FRESH loader — the end-to-end proof that the write half
/// and the read half agree, driven through the real `from_vars`.
#[test]
fn a_persisted_grant_is_picked_up_by_a_fresh_loader() {
    let dir = tempfile::tempdir().unwrap();
    seeded_store(
        dir.path(),
        &[
            ("CTRADER_CLIENT_ID", "app"),
            ("CTRADER_CLIENT_SECRET", "sec"),
            ("CTRADER_DEMO_ACCESS_TOKEN", "AT_stale"),
            ("CTRADER_DEMO_REFRESH_TOKEN", "RT_stale"),
        ],
    );

    let p = demo_persist(dir.path());
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
/// a grant the venue has already exchanged must not be "saved" somewhere nothing reads.
#[test]
fn a_persist_failure_names_the_path_and_keys_but_never_the_token() {
    let dir = tempfile::tempdir().unwrap();
    // No settings database: there is nowhere to write.
    let p = demo_persist(dir.path());
    let err = persist(&p, &token("AT_SECRET_LEAK", "RT_SECRET_LEAK", 10), T)
        .expect_err("with no settings database there is no store to write");
    let msg = err.to_string();
    assert!(msg.contains(&p.store().display().to_string()), "{msg}");
    assert!(msg.contains("CTRADER_DEMO_ACCESS_TOKEN"), "{msg}");
    assert!(msg.contains("secrets init"), "the refusal must name the way in: {msg}");
    assert!(!msg.contains("AT_SECRET_LEAK"), "leaked the access token: {msg}");
    assert!(!msg.contains("RT_SECRET_LEAK"), "leaked the refresh token: {msg}");
    assert!(
        !vike_secrets::db_path_in(dir.path()).exists(),
        "a refused rotation must not bring a store into being"
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
