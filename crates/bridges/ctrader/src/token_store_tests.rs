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

/// THE property the whole change rests on: a refreshed pair lands on its two keys and EVERY
/// other line of the store is byte-identical — comments, blank lines, other venues' keys and
/// their order. This is the store-side twin of `vike_secrets::env_write`'s own suite, asserted
/// here through the REAL file write so the atomic path is covered too.
#[test]
fn a_refresh_rewrites_only_its_two_keys_and_preserves_every_other_line() {
    let dir = tempfile::tempdir().unwrap();
    let store = dir.path().join("secrets.env");
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
    std::fs::write(&store, original).unwrap();

    let p = demo_persist(dir.path(), store.clone());
    persist(&p, &token("AT_fresh", "RT_fresh", 2_592_000), T).unwrap();

    let after = std::fs::read_to_string(&store).unwrap();
    let before_lines: Vec<&str> = original.lines().collect();
    let after_lines: Vec<&str> = after.lines().collect();
    assert_eq!(before_lines.len(), after_lines.len(), "no line may be added or dropped");

    for (i, (b, a)) in before_lines.iter().zip(after_lines.iter()).enumerate() {
        if b.starts_with("CTRADER_DEMO_ACCESS_TOKEN") || b.starts_with("CTRADER_DEMO_REFRESH_TOKEN")
        {
            continue;
        }
        assert_eq!(b, a, "line {i} must be byte-identical: {b:?} -> {a:?}");
    }
    assert_eq!(after_lines[7], "CTRADER_DEMO_ACCESS_TOKEN=AT_fresh");
    assert_eq!(after_lines[8], "CTRADER_DEMO_REFRESH_TOKEN=RT_fresh");
    assert!(!after.contains("AT_stale"), "the spent access token must not survive");
    assert!(!after.contains("RT_stale"), "the spent refresh token must not survive");
}

/// A persisted grant is picked up by a FRESH loader — the end-to-end proof that the write half
/// and the read half agree, driven through the real `from_vars`.
#[test]
fn a_persisted_grant_is_picked_up_by_a_fresh_loader() {
    let dir = tempfile::tempdir().unwrap();
    let store = dir.path().join("secrets.env");
    std::fs::write(
        &store,
        "CTRADER_CLIENT_ID=app\nCTRADER_CLIENT_SECRET=sec\n\
             CTRADER_DEMO_ACCESS_TOKEN=AT_stale\nCTRADER_DEMO_REFRESH_TOKEN=RT_stale\n",
    )
    .unwrap();

    let p = demo_persist(dir.path(), store.clone());
    persist(&p, &token("AT_rotated", "RT_rotated", 100), T).unwrap();

    // A cold start: re-read the store exactly as a composition root would, then load the venue
    // config from it.
    let vars = vike_secrets::parse_dotenv(&std::fs::read_to_string(&store).unwrap());
    let cfg = crate::config::CtraderConfig::from_vars(Environment::Demo, &vars)
        .expect("the rotated grant gates live");
    assert_eq!(cfg.access_token, "AT_rotated");
    assert_eq!(cfg.refresh_token, "RT_rotated");
}

/// An unwritable store is an ERROR that names the path and the keys, and never the token.
#[test]
fn a_persist_failure_names_the_path_and_keys_but_never_the_token() {
    let dir = tempfile::tempdir().unwrap();
    // A DIRECTORY where the store should be: the write cannot succeed.
    let store = dir.path().join("secrets.env");
    std::fs::create_dir(&store).unwrap();

    let p = demo_persist(dir.path(), store.clone());
    let err = persist(&p, &token("AT_SECRET_LEAK", "RT_SECRET_LEAK", 10), T)
        .expect_err("a directory is not writable as a file");
    let msg = err.to_string();
    assert!(msg.contains(&store.display().to_string()), "{msg}");
    assert!(msg.contains("CTRADER_DEMO_ACCESS_TOKEN"), "{msg}");
    assert!(!msg.contains("AT_SECRET_LEAK"), "leaked the access token: {msg}");
    assert!(!msg.contains("RT_SECRET_LEAK"), "leaked the refresh token: {msg}");
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
