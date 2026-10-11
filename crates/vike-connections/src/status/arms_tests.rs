//! Per-venue detection tests: each bespoke key shape in `arms` lights exactly the tiers its bridge reads.

use super::*;
use crate::status::credential_status;

/// FXCM's bespoke `_USER`/`_PASSWORD` shape: the generic `_API_KEY`/`_API_SECRET` check
/// would always report "not set" for it, so this override must detect the real vars.
#[test]
fn fxcm_demo_user_password_detected() {
    let mut vars = HashMap::new();
    vars.insert("FXCM_DEMO_USER".to_string(), "D251112911".to_string());
    vars.insert("FXCM_DEMO_PASSWORD".to_string(), "s3cr3t".to_string());
    let statuses = credential_status(&vars);
    let fxcm = statuses.iter().find(|s| s.venue == "fxcm").expect("fxcm present");
    assert!(fxcm.demo, "fxcm demo should be detected via FXCM_DEMO_USER/_PASSWORD");
    assert!(!fxcm.sim);
    assert!(!fxcm.live);
}

/// `MAINNET` is no credential tier (owner ruling 2026-10-09): a `MAINNET`-spelled FXCM login
/// lights NO tier, mirroring the loader, which reads it at none. Names composed, never spelled —
/// this module folds into `arms.rs` for the settings registry's literal sweep.
#[test]
fn fxcm_mainnet_spelling_lights_no_tier() {
    let mut vars = HashMap::new();
    vars.insert(format!("FXCM_{}_USER", "MAINNET"), "u".to_string());
    vars.insert(format!("FXCM_{}_PASSWORD", "MAINNET"), "p".to_string());
    let statuses = credential_status(&vars);
    let fxcm = statuses.iter().find(|s| s.venue == "fxcm").expect("fxcm present");
    assert!(!fxcm.live && !fxcm.demo && !fxcm.sim, "{fxcm:?}");
}

/// OANDA sets `_API_KEY` but not `_API_SECRET` — the generic check would always miss it
/// even with real credentials present. The override must check `_ACCOUNT_ID` instead.
#[test]
fn oanda_api_key_and_account_id_detected() {
    let mut vars = HashMap::new();
    vars.insert("OANDA_DEMO_API_KEY".to_string(), "tok-abc-123".to_string());
    vars.insert("OANDA_DEMO_ACCOUNT_ID".to_string(), "101-004-1234567-001".to_string());
    let statuses = credential_status(&vars);
    let oanda = statuses.iter().find(|s| s.venue == "oanda").expect("oanda present");
    assert!(oanda.demo);
    // the API key alone (no account id) must NOT count as configured
    let mut key_only = HashMap::new();
    key_only.insert("OANDA_DEMO_API_KEY".to_string(), "tok-abc-123".to_string());
    let statuses = credential_status(&key_only);
    let oanda = statuses.iter().find(|s| s.venue == "oanda").expect("oanda present");
    assert!(!oanda.demo, "api key alone (no account id) must not count as configured");
}

/// IG needs all three of `_API_KEY`/`_IDENTIFIER`/`_PASSWORD`.
#[test]
fn ig_all_three_fields_required() {
    let mut vars = HashMap::new();
    vars.insert("IG_DEMO_API_KEY".to_string(), "key-xyz".to_string());
    vars.insert("IG_DEMO_IDENTIFIER".to_string(), "id".to_string());
    let statuses = credential_status(&vars);
    let ig = statuses.iter().find(|s| s.venue == "ig").expect("ig present");
    assert!(!ig.demo, "missing password must not count as configured");

    vars.insert("IG_DEMO_PASSWORD".to_string(), "pw".to_string());
    let statuses = credential_status(&vars);
    let ig = statuses.iter().find(|s| s.venue == "ig").expect("ig present");
    assert!(ig.demo);
}

/// Dukascopy has no Sim/Live tier at all; Demo is true when either DEMO1 or DEMO2 is set.
#[test]
fn dukascopy_demo1_and_demo2_detected_no_sim_or_live() {
    let mut vars = HashMap::new();
    vars.insert("DUKASCOPY_DEMO1_LOGIN".to_string(), "DEMO2cGyrc".to_string());
    vars.insert("DUKASCOPY_DEMO1_PASSWORD".to_string(), "s3cr3t".to_string());
    let statuses = credential_status(&vars);
    let duka = statuses.iter().find(|s| s.venue == "dukascopy").expect("dukascopy present");
    assert!(duka.demo);
    assert!(!duka.sim, "dukascopy has no SIM tier");
    assert!(!duka.live, "dukascopy has no LIVE tier");

    let mut demo2 = HashMap::new();
    demo2.insert("DUKASCOPY_DEMO2_LOGIN".to_string(), "u".to_string());
    demo2.insert("DUKASCOPY_DEMO2_PASSWORD".to_string(), "p".to_string());
    let statuses = credential_status(&demo2);
    let duka = statuses.iter().find(|s| s.venue == "dukascopy").expect("dukascopy present");
    assert!(duka.demo, "DEMO2 alone should also be detected");
}

/// Aster's `Demo` tier reads `ASTER_TESTNET_*`, not `ASTER_DEMO_*` — the bridge's own loader
/// naming, not the generic app-tier naming.
#[test]
fn aster_testnet_user_and_private_key_detected() {
    let mut vars = HashMap::new();
    vars.insert("ASTER_TESTNET_USER".to_string(), "0xUser".to_string());
    vars.insert("ASTER_TESTNET_PRIVATE_KEY".to_string(), "0xkey".to_string());
    let statuses = credential_status(&vars);
    let aster = statuses.iter().find(|s| s.venue == "aster").expect("aster present");
    assert!(aster.demo);
    assert!(!aster.sim);
    assert!(!aster.live);
}

/// Aster's `Live` tier reads `ASTER_LIVE_*`.
#[test]
fn aster_live_user_and_private_key_detected() {
    let mut vars = HashMap::new();
    vars.insert("ASTER_LIVE_USER".to_string(), "0xUser".to_string());
    vars.insert("ASTER_LIVE_PRIVATE_KEY".to_string(), "0xkey".to_string());
    let statuses = credential_status(&vars);
    let aster = statuses.iter().find(|s| s.venue == "aster").expect("aster present");
    assert!(aster.live);
    assert!(!aster.demo);
}

/// `USER` alone (no `PRIVATE_KEY`) must not count as configured; `SIGNER` is optional but
/// `PRIVATE_KEY` is not.
#[test]
fn aster_user_alone_is_not_configured() {
    let mut vars = HashMap::new();
    vars.insert("ASTER_TESTNET_USER".to_string(), "0xUser".to_string());
    let statuses = credential_status(&vars);
    let aster = statuses.iter().find(|s| s.venue == "aster").expect("aster present");
    assert!(!aster.demo, "private key missing must not count as configured");
}

/// Aster has no `Sim` tier — `sim` must stay `false` even if `ASTER_SIM_*` vars happen to be
/// set (mirrors dukascopy's no-`Sim`/`Live`-tier shape).
#[test]
fn aster_sim_never_configured() {
    let mut vars = HashMap::new();
    vars.insert("ASTER_SIM_USER".to_string(), "0xUser".to_string());
    vars.insert("ASTER_SIM_PRIVATE_KEY".to_string(), "0xkey".to_string());
    let statuses = credential_status(&vars);
    let aster = statuses.iter().find(|s| s.venue == "aster").expect("aster present");
    assert!(!aster.sim, "aster has no SIM tier");
}

/// Hyperliquid appears in the grid at all (it was missing from the catalog-side roster until the
/// #498 roster audit), and its bespoke `HYPERLIQUID_{DEMO|LIVE}_PRIVATE_KEY` shape is
/// detected — the generic `_API_KEY`/`_API_SECRET` check would always report "not set".
/// `_ACCOUNT_ADDRESS` is optional (agent-wallet mode) and must NOT be required.
#[test]
fn hyperliquid_private_key_detected_per_tier() {
    let mut vars = HashMap::new();
    vars.insert("HYPERLIQUID_DEMO_PRIVATE_KEY".to_string(), "0xabc123".to_string());
    let statuses = credential_status(&vars);
    let hl = statuses.iter().find(|s| s.venue == "hyperliquid").expect("hyperliquid present");
    assert!(hl.demo, "demo private key alone (no account address) should be detected");
    assert!(!hl.live);
    assert!(!hl.sim);

    vars.insert("HYPERLIQUID_LIVE_PRIVATE_KEY".to_string(), "0xdeadbeef".to_string());
    let statuses = credential_status(&vars);
    let hl = statuses.iter().find(|s| s.venue == "hyperliquid").expect("hyperliquid present");
    assert!(hl.live);
}

/// Hyperliquid has no `Sim` tier — `sim` must stay `false` even if `HYPERLIQUID_SIM_*` vars
/// happen to be set (mirrors dukascopy/aster).
#[test]
fn hyperliquid_sim_never_configured() {
    let mut vars = HashMap::new();
    vars.insert("HYPERLIQUID_SIM_PRIVATE_KEY".to_string(), "0xkey".to_string());
    let statuses = credential_status(&vars);
    let hl = statuses.iter().find(|s| s.venue == "hyperliquid").expect("hyperliquid present");
    assert!(!hl.sim, "hyperliquid has no SIM tier");
}
