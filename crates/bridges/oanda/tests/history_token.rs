//! The credential half of OANDA's HISTORY lane: which store names a SCOPED read declares, and how
//! the one token is read back out of what that read returned.
//!
//! The datahub builds `KeyScope::of(oanda_history_token_names())`, reads the store through it and
//! hands the resulting map to `load_oanda_history_token`. Everything here is over plain maps — no
//! store, no network, no real credential (every value is an obviously fake constant).

use std::collections::HashMap;

use vike_bridge_core::credentials::{Environment, KeyScope};
use vike_model::account_keys::{AccountLabel, account_key};
use vike_oanda::{
    MountableTier, load_oanda_config_from, load_oanda_history_token, mountable_tier,
    oanda_env_var_names, oanda_history_token_names,
};

fn vars(pairs: &[(&str, &str)]) -> HashMap<String, String> {
    pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect()
}

const PRACTICE_KEY: &str = "OANDA_DEMO_API_KEY";
const PRACTICE_ACCOUNT: &str = "OANDA_DEMO_ACCOUNT_ID";

/// The scope is the practice tier's API key and NOTHING else: not the account id, not a live-tier
/// name in either spelling, not the sim tier.
#[test]
fn the_scope_declares_the_practice_api_key_and_nothing_else() {
    let names = oanda_history_token_names();
    assert_eq!(names, [PRACTICE_KEY]);

    let scope = KeyScope::of(&names);
    assert_eq!(scope.len(), 1);
    assert!(scope.declares(PRACTICE_KEY));
    for other in [
        PRACTICE_ACCOUNT,
        "OANDA_LIVE_API_KEY",
        "OANDA_LIVE_ACCOUNT_ID",
        "OANDA_MAINNET_API_KEY",
        "OANDA_MAINNET_ACCOUNT_ID",
        "OANDA_SIM_API_KEY",
    ] {
        assert!(!scope.declares(other), "{other} must be outside the history scope");
    }
}

/// Composed through the crate's single naming site: the scope's name IS the practice API key that
/// `oanda_env_var_names` (and so `load_oanda_config_from`) spells, so a tier rename moves them
/// together.
#[test]
fn the_declared_name_is_the_one_the_config_loader_reads() {
    assert_eq!(oanda_history_token_names(), [oanda_env_var_names(Environment::Demo).0]);
}

/// A candles read is account-free: a store holding the token and no account id serves history and
/// arms nothing — the very case the config loader (which needs both) refuses.
#[test]
fn the_token_needs_no_account_id() {
    let map = vars(&[(PRACTICE_KEY, "tok-abc")]);
    assert_eq!(load_oanda_history_token(&map).as_deref(), Some("tok-abc"));
    assert!(
        load_oanda_config_from(Environment::Demo, &map).is_none(),
        "the control: the config loader needs the account id too, so this store arms nothing"
    );
}

#[test]
fn an_absent_or_blank_token_is_none() {
    assert_eq!(load_oanda_history_token(&HashMap::new()), None);
    assert_eq!(load_oanda_history_token(&vars(&[(PRACTICE_KEY, "")])), None);
    assert_eq!(load_oanda_history_token(&vars(&[(PRACTICE_KEY, "   \t ")])), None);
    // An account id alone is not a token.
    assert_eq!(load_oanda_history_token(&vars(&[(PRACTICE_ACCOUNT, "101-004-1-001")])), None);
}

#[test]
fn the_token_is_trimmed() {
    let map = vars(&[(PRACTICE_KEY, "  tok-abc  ")]);
    assert_eq!(load_oanda_history_token(&map).as_deref(), Some("tok-abc"));
}

/// **A live-named key is IGNORED, not an error** — the opposite disposition from `mountable_tier`,
/// on purpose: that refusal protects ORDERS from silently trading the wrong account, and history
/// places none. The last assertion pins the contrast so neither can drift into the other.
#[test]
fn a_live_named_key_is_ignored_never_read_and_never_an_error() {
    // Live-named keys alone: there is no practice token, and that is all that is said.
    for pair in [
        [("OANDA_LIVE_API_KEY", "live-tok"), ("OANDA_LIVE_ACCOUNT_ID", "001-001-1-001")],
        [("OANDA_MAINNET_API_KEY", "legacy-tok"), ("OANDA_MAINNET_ACCOUNT_ID", "001-001-2-001")],
    ] {
        assert_eq!(load_oanda_history_token(&vars(&pair)), None, "{pair:?}");
    }

    // Both tiers: the practice token comes back, untouched by its live-named neighbours.
    let both = vars(&[
        (PRACTICE_KEY, "demo-tok"),
        (PRACTICE_ACCOUNT, "101-004-1-001"),
        ("OANDA_LIVE_API_KEY", "live-tok"),
        ("OANDA_LIVE_ACCOUNT_ID", "001-001-1-001"),
        ("OANDA_MAINNET_API_KEY", "legacy-tok"),
    ]);
    assert_eq!(load_oanda_history_token(&both).as_deref(), Some("demo-tok"));

    // The contrast: the SAME store makes `mountable_tier` refuse to arm anything.
    assert!(matches!(mountable_tier(&both), MountableTier::LiveUnreachable(_)));
}

/// Only names inside the declared scope are ever consulted: take the declared names out of a map
/// that holds every other OANDA-shaped key, and nothing is left to read.
#[test]
fn the_loader_reads_only_inside_the_scope() {
    let mut map = vars(&[
        (PRACTICE_ACCOUNT, "101-004-1-001"),
        ("OANDA_LIVE_API_KEY", "live-tok"),
        ("OANDA_LIVE_ACCOUNT_ID", "001-001-1-001"),
        ("OANDA_MAINNET_API_KEY", "legacy-tok"),
        ("OANDA_MAINNET_ACCOUNT_ID", "001-001-2-001"),
        ("OANDA_SIM_API_KEY", "sim-tok"),
        ("OANDA_SIM_ACCOUNT_ID", "001-001-3-001"),
    ]);
    // Composed, never spelled, so no account separator lands in a bridge literal.
    let alt = AccountLabel::parse("ALT").expect("a legal label");
    map.insert(account_key(PRACTICE_KEY, &alt), "alt-tok".to_string());
    assert_eq!(load_oanda_history_token(&map), None);

    // …and each declared name, added back, is what answers.
    for name in oanda_history_token_names() {
        map.insert(name, "tok-in-scope".to_string());
    }
    assert_eq!(load_oanda_history_token(&map).as_deref(), Some("tok-in-scope"));
}

/// A labelled account's key is not the default account's token, and no fallback crosses the two —
/// the same "never borrow another account's credential" rule the config loader keeps.
#[test]
fn a_labelled_accounts_key_is_not_the_default_token() {
    let alt = AccountLabel::parse("ALT").expect("a legal label");
    let mut map = HashMap::new();
    map.insert(account_key(PRACTICE_KEY, &alt), "alt-tok".to_string());
    assert_eq!(load_oanda_history_token(&map), None);
}

/// When both are configured the history loader finds the token the config loader finds — it is the
/// same key, read by the same rule, not a second opinion.
#[test]
fn it_finds_the_token_the_config_loader_finds() {
    let map = vars(&[(PRACTICE_KEY, " tok-abc "), (PRACTICE_ACCOUNT, "101-004-1-001")]);
    let cfg = load_oanda_config_from(Environment::Demo, &map).expect("both fields present");
    assert_eq!(load_oanda_history_token(&map), Some(cfg.api_token));
}
