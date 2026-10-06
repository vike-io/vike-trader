//! Integration coverage for `CtraderConfig`'s credential gate. Mirrors
//! `crates/bridges/oanda/tests/config_env.rs`'s HashMap-driven style: `from_vars` is the
//! deterministic unit under test (no process-env mutation, so nothing to serialize/race across
//! tests — `std::env::set_var` is process-global and NOT thread-safe to mutate from parallel
//! `#[test]` functions). `from_vars` is also the ONLY entry point — the former `from_env`
//! (a `load_workspace_dotenv()` self-sweep) was deleted when `CtraderCatalog::new` moved to the
//! caller-supplied vars map (the settings-registry convention: the caller owns the `.env` I/O).

use std::collections::HashMap;

use vike_bridge_core::credentials::Environment;
use vike_ctrader::config::CtraderConfig;

fn full_demo_vars() -> HashMap<String, String> {
    let mut vars = HashMap::new();
    vars.insert("CTRADER_CLIENT_ID".into(), "cid-123".into());
    vars.insert("CTRADER_CLIENT_SECRET".into(), "super-secret-client-secret".into());
    vars.insert("CTRADER_DEMO_ACCESS_TOKEN".into(), "super-secret-access-token".into());
    vars.insert("CTRADER_DEMO_REFRESH_TOKEN".into(), "super-secret-refresh-token".into());
    vars
}

#[test]
fn all_vars_unset_gates_to_none() {
    let vars = HashMap::new();
    assert!(CtraderConfig::from_vars(Environment::Demo, &vars).is_none());
}

#[test]
fn full_demo_credentials_load_with_the_demo_host_and_port() {
    let vars = full_demo_vars();
    let cfg = CtraderConfig::from_vars(Environment::Demo, &vars).expect("must load");
    assert_eq!(cfg.host, "demo.ctraderapi.com");
    assert_eq!(cfg.port, 5035);
    assert_eq!(cfg.account_id, None, "account id is optional — discovered at connect time");
}

#[test]
fn live_credentials_resolve_to_the_live_host() {
    let mut vars = full_demo_vars();
    vars.insert("CTRADER_LIVE_ACCESS_TOKEN".into(), "live-token".into());
    vars.insert("CTRADER_LIVE_REFRESH_TOKEN".into(), "live-refresh".into());
    let cfg = CtraderConfig::from_vars(Environment::Live, &vars).expect("must load");
    assert_eq!(cfg.host, "live.ctraderapi.com");
    assert_eq!(cfg.port, 5035);
}

#[test]
fn debug_never_leaks_the_client_secret_or_either_token() {
    let vars = full_demo_vars();
    let cfg = CtraderConfig::from_vars(Environment::Demo, &vars).unwrap();
    let debug = format!("{cfg:?}");
    assert!(!debug.contains("super-secret-client-secret"), "leaked client_secret: {debug}");
    assert!(!debug.contains("super-secret-access-token"), "leaked access_token: {debug}");
    assert!(!debug.contains("super-secret-refresh-token"), "leaked refresh_token: {debug}");
    assert!(debug.contains("cid-123"), "client_id should stay visible: {debug}");
    assert!(debug.contains("demo.ctraderapi.com"));
}

#[test]
fn each_required_field_missing_alone_gates_to_none() {
    for missing in [
        "CTRADER_CLIENT_ID",
        "CTRADER_CLIENT_SECRET",
        "CTRADER_DEMO_ACCESS_TOKEN",
        "CTRADER_DEMO_REFRESH_TOKEN",
    ] {
        let mut vars = full_demo_vars();
        vars.remove(missing);
        assert!(
            CtraderConfig::from_vars(Environment::Demo, &vars).is_none(),
            "missing {missing} must gate to None"
        );
    }
}

#[test]
fn account_id_is_optional_but_parsed_when_present() {
    let mut vars = full_demo_vars();
    vars.insert("CTRADER_DEMO_ACCOUNT_ID".into(), "42".into());
    let cfg = CtraderConfig::from_vars(Environment::Demo, &vars).unwrap();
    assert_eq!(cfg.account_id, Some(42));
}

#[test]
fn blank_values_are_treated_as_absent() {
    let mut vars = full_demo_vars();
    vars.insert("CTRADER_CLIENT_SECRET".into(), "   ".into());
    assert!(CtraderConfig::from_vars(Environment::Demo, &vars).is_none());
}

#[test]
fn values_are_trimmed() {
    let mut vars = full_demo_vars();
    vars.insert("CTRADER_CLIENT_ID".into(), "  cid-123  ".into());
    let cfg = CtraderConfig::from_vars(Environment::Demo, &vars).unwrap();
    assert_eq!(cfg.client_id, "cid-123");
}

#[test]
fn to_conn_config_carries_the_refresh_token_and_account_id() {
    let mut vars = full_demo_vars();
    vars.insert("CTRADER_DEMO_ACCOUNT_ID".into(), "42".into());
    let cfg = CtraderConfig::from_vars(Environment::Demo, &vars).unwrap();
    let conn_cfg = cfg.to_conn_config();
    assert_eq!(conn_cfg.host, "demo.ctraderapi.com");
    assert_eq!(conn_cfg.port, 5035);
    assert_eq!(conn_cfg.account_id, Some(42));
    assert_eq!(conn_cfg.refresh_token.as_deref(), Some("super-secret-refresh-token"));
    assert!(!conn_cfg.no_tls, "production config must use TLS");
}
