//! **A venue-rotated cTrader grant lands in the change journal** — ONE record for the PAIR, with
//! the `venue` origin, and not a byte of either token.
//!
//! This call site is the reason `vike_model::change_journal::Actor::Venue` exists, and it is also
//! the one that was invisible everywhere: `crate::token_store` logs nothing (still true, apart from
//! a journal-append failure), so before this a daemon could rotate a live OAuth grant and leave no
//! trace on any surface in the workspace.
//!
//! ⚠ **The pair is ONE change, not two.** cTrader rotates the refresh token alongside the access
//! token — that is why `crate::token_store::persist` writes both in a single upsert, since a
//! half-write leaves a SPENT refresh token on disk. Two records would read as two rotations and
//! invite exactly that misreading back, so `count == 2` on one line is the gated shape.
//!
//! Everything is driven through the REAL `crate::config::CtraderConfig::from_vars_with_store`, so
//! this also gates that the composition root's ONE `state_dir` parameter produces BOTH the store
//! path and the ledger location. A test that hand-built a `TokenPersist` could not tell a wired
//! derivation from two independent ones.

use std::collections::HashMap;
use std::path::Path;

use vike_bridge_core::credentials::Environment;
use vike_ctrader::config::CtraderConfig;
use vike_ctrader::oauth::Token;
use vike_ctrader::token_store::persist;
use vike_model::change_journal::{CHANGES_SUBDIR, KIND_CREDENTIAL_WRITE, month_file_name};

/// 2026-08-21T00:00:00Z — the injected instant. `persist` takes it as a parameter because
/// `vike_model::change_journal` reads no clock.
const T: i64 = 1_787_356_800_000;

/// The rotated pair. ⚠ Deliberately sharing no six-character run with anything a `credential_write`
/// line legitimately holds (`ctrader`, `secrets.env`, `CTRADER_DEMO_ACCESS_TOKEN`, the digits of
/// [`T`]) — otherwise [`assert_no_secret_window`] would be testing coincidence rather than leakage.
const FRESH_ACCESS: &str = "qzjvwx7413mfbphgnd8256wu";
const FRESH_REFRESH: &str = "gfpzmwqx9042hvbjntdu6531";

/// The shortest run of a token this test refuses to find in the ledger. Half a live grant is still a
/// live grant, so a whole-string `contains` check would be the wrong bound.
const MIN_LEAK_WINDOW: usize = 6;

fn vars(access: &str, refresh: &str) -> HashMap<String, String> {
    [
        ("CTRADER_CLIENT_ID", "app-id"),
        ("CTRADER_CLIENT_SECRET", "app-secret"),
        ("CTRADER_DEMO_ACCESS_TOKEN", access),
        ("CTRADER_DEMO_REFRESH_TOKEN", refresh),
    ]
    .into_iter()
    .map(|(k, v)| (k.to_string(), v.to_string()))
    .collect()
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
fn one_rotation_is_one_venue_record_naming_both_keys_and_neither_token() {
    let dir = tempfile::tempdir().unwrap();
    let settings = dir.path().join("settings");
    let state = settings.join("state");
    std::fs::create_dir_all(&state).unwrap();
    let store = settings.join("secrets.env");
    std::fs::write(
        &store,
        "# keep me\nCTRADER_CLIENT_ID=app-id\nCTRADER_CLIENT_SECRET=app-secret\n\
         CTRADER_DEMO_ACCESS_TOKEN=AT_stale\nCTRADER_DEMO_REFRESH_TOKEN=RT_stale\n",
    )
    .unwrap();

    // The REAL config path: one `state_dir` in, both the store and the ledger location out.
    let cfg = CtraderConfig::from_vars_with_store(
        Environment::Demo,
        &vars("AT_stale", "RT_stale"),
        Some(&state),
    )
    .expect("a complete demo grant gates live");
    let p = cfg.token_persist.expect("a declared state dir yields a persist target");
    assert_eq!(p.store, store, "the store is DERIVED from the same state dir, not re-walked");
    assert_eq!(p.state_dir, state, "…and so is the ledger's home");

    persist(&p, &token(FRESH_ACCESS, FRESH_REFRESH), T).expect("persist");

    // Anti-vacuity FIRST: the rotation really landed. Without this, every assertion below would
    // stay green for a `persist` that had stopped writing the store entirely.
    let saved = std::fs::read_to_string(&store).unwrap();
    assert!(saved.contains(&format!("CTRADER_DEMO_ACCESS_TOKEN={FRESH_ACCESS}")), "{saved}");
    assert!(saved.contains(&format!("CTRADER_DEMO_REFRESH_TOKEN={FRESH_REFRESH}")), "{saved}");
    assert!(saved.contains("# keep me"), "the upsert preserves every other line: {saved}");

    let ledger = state.join(CHANGES_SUBDIR).join(month_file_name(T));
    let raw = std::fs::read_to_string(&ledger).expect("the change journal was written");
    assert_eq!(raw.lines().count(), 1, "ONE rotation is ONE record, never one per key: {raw}");

    let v: serde_json::Value = serde_json::from_str(raw.trim_end()).expect("one JSON object");
    assert_eq!(v["kind"], KIND_CREDENTIAL_WRITE);
    assert_eq!(v["outcome"], "applied");
    // The VENUE rotated this, not a human at a keyboard — the whole reason that origin exists.
    assert_eq!(v["actor"]["origin"], "venue", "{raw}");
    assert_eq!(v["actor"]["venue"], "ctrader", "{raw}");
    assert!(v["actor"].get("user").is_none(), "no invented human actor: {raw}");

    assert_eq!(v["target"]["store"], "secrets.env");
    assert_eq!(v["target"]["venue"], "ctrader");
    assert_eq!(v["target"]["tier"], "DEMO", "the tier is read back off the key names: {raw}");
    assert_eq!(v["target"]["count"], 2, "the access token and the refresh token, together");
    let keys: Vec<&str> =
        v["target"]["keys"].as_array().unwrap().iter().map(|k| k.as_str().unwrap()).collect();
    assert_eq!(keys, vec!["CTRADER_DEMO_ACCESS_TOKEN", "CTRADER_DEMO_REFRESH_TOKEN"]);

    assert_no_secret_window(&raw, FRESH_ACCESS);
    assert_no_secret_window(&raw, FRESH_REFRESH);
}

/// The LIVE tier's record says `LIVE`, so a reader can tell a demo rotation from a real one.
///
/// Cheap, and it is the assertion that stops `tier_of` being pinned by a single fixture: a
/// hard-coded `"DEMO"` would pass the test above and be wrong about every production rotation.
#[test]
fn a_live_tier_rotation_records_the_live_tier() {
    let dir = tempfile::tempdir().unwrap();
    let state = dir.path().join("settings").join("state");
    std::fs::create_dir_all(&state).unwrap();

    let live_vars: HashMap<String, String> = [
        ("CTRADER_CLIENT_ID", "app-id"),
        ("CTRADER_CLIENT_SECRET", "app-secret"),
        ("CTRADER_LIVE_ACCESS_TOKEN", "AT_stale"),
        ("CTRADER_LIVE_REFRESH_TOKEN", "RT_stale"),
    ]
    .into_iter()
    .map(|(k, v)| (k.to_string(), v.to_string()))
    .collect();

    let cfg = CtraderConfig::from_vars_with_store(Environment::Live, &live_vars, Some(&state))
        .expect("a complete live grant gates live");
    let p = cfg.token_persist.expect("a persist target");
    persist(&p, &token(FRESH_ACCESS, FRESH_REFRESH), T).expect("persist");

    let raw = std::fs::read_to_string(state.join(CHANGES_SUBDIR).join(month_file_name(T))).unwrap();
    let v: serde_json::Value = serde_json::from_str(raw.trim_end()).unwrap();
    assert_eq!(v["target"]["tier"], "LIVE", "{raw}");
    assert_eq!(v["target"]["count"], 2);
}

/// **No state directory ⇒ nothing is written and no path is invented** — not the store, not a
/// ledger.
///
/// A mount whose composition root resolved no project has nowhere legitimate to put either, and
/// `CtraderConfig::from_vars` (the `state_dir`-less spelling every test and the catalog probe use)
/// is the shape that says so: no `token_persist`, so `crate::conn`'s `adopt_refreshed_token` takes
/// its early-return arm and the refresh lives as long as the process. Asserted against a directory
/// tree that stays EMPTY, so "wrote nothing" and "wrote somewhere else" are distinguishable.
#[test]
fn a_stateless_mount_persists_nothing_and_invents_no_ledger() {
    let dir = tempfile::tempdir().unwrap();
    let before: Vec<_> = std::fs::read_dir(dir.path()).unwrap().flatten().collect();
    assert!(before.is_empty(), "precondition: the tree starts empty");

    let cfg = CtraderConfig::from_vars(Environment::Demo, &vars("AT_stale", "RT_stale"))
        .expect("the grant still gates live — only the rotation home is absent");
    assert!(
        cfg.token_persist.is_none(),
        "no state dir means no store to rotate into, and therefore no ledger to record into"
    );

    // …and nothing appeared anywhere: not a `changes/`, not a `state/`, not a `secrets.env`.
    let after: Vec<_> = std::fs::read_dir(dir.path()).unwrap().flatten().collect();
    assert!(after.is_empty(), "a stateless mount wrote something: {after:?}");
    assert!(!Path::new(&dir.path().join(CHANGES_SUBDIR)).exists());
}
