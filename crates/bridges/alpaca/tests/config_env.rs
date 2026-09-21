use std::collections::HashMap;
use vike_alpaca::{alpaca_tier, load_alpaca_config_from};
use vike_bridge_core::credentials::Environment;

#[test]
fn tier_names() {
    assert_eq!(alpaca_tier(Environment::Demo), "SANDBOX");
    assert_eq!(alpaca_tier(Environment::Sim), "SANDBOX");
    assert_eq!(alpaca_tier(Environment::Live), "LIVE");
}

#[test]
fn gate_requires_all_three_fields() {
    let mut vars = HashMap::new();
    assert!(load_alpaca_config_from(Environment::Demo, &vars).is_none());
    vars.insert("ALPACA_SANDBOX_CLIENT_ID".into(), "CID123".into());
    assert!(load_alpaca_config_from(Environment::Demo, &vars).is_none()); // secret+account missing
    vars.insert("ALPACA_SANDBOX_CLIENT_SECRET".into(), "SEC456".into());
    assert!(load_alpaca_config_from(Environment::Demo, &vars).is_none()); // account missing
    vars.insert("ALPACA_SANDBOX_ACCOUNT_ID".into(), "acct-789".into());
    let c = load_alpaca_config_from(Environment::Demo, &vars).unwrap();
    assert_eq!(c.client_id, "CID123");
    assert_eq!(c.account_id, "acct-789");
    assert!(c.hosts.broker.contains("sandbox"));
}

#[test]
fn secret_never_leaks_in_debug() {
    let mut vars = HashMap::new();
    vars.insert("ALPACA_SANDBOX_CLIENT_ID".into(), "CID123".into());
    vars.insert("ALPACA_SANDBOX_CLIENT_SECRET".into(), "topsecret-DO-NOT-LEAK".into());
    vars.insert("ALPACA_SANDBOX_ACCOUNT_ID".into(), "acct-789".into());
    let c = load_alpaca_config_from(Environment::Demo, &vars).unwrap();
    let dbg = format!("{c:?}");
    assert!(!dbg.contains("topsecret-DO-NOT-LEAK"), "secret leaked in Debug: {dbg}");
}

#[test]
fn blank_values_are_treated_as_absent() {
    let mut vars = HashMap::new();
    vars.insert("ALPACA_SANDBOX_CLIENT_ID".into(), "   ".into());
    vars.insert("ALPACA_SANDBOX_CLIENT_SECRET".into(), "SEC".into());
    vars.insert("ALPACA_SANDBOX_ACCOUNT_ID".into(), "acct".into());
    assert!(load_alpaca_config_from(Environment::Demo, &vars).is_none());
}
