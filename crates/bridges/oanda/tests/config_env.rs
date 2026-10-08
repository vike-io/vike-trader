//! Integration coverage for OANDA's bespoke `.env` credential scheme (`oanda_env_var_names`,
//! `load_oanda_config_from`) — bearer-token + account-id, NOT the HMAC `Credentials` shape every
//! other venue uses. The inline unit test in `config.rs` only exercises the Demo tier; this adds
//! the Live legacy-MAINNET fallback (mirroring
//! `vike-bridge-core/src/credentials.rs::live_tier_tests`, which OANDA's loader does NOT reuse —
//! it has its own copy of the tier-fallback logic), the partial-credential gate, and
//! `mountable_tier` — the refusal a LIVE-named store earns, since this workspace can reach no
//! fxTrade tier at all.

use std::collections::HashMap;

use vike_bridge_core::credentials::Environment;
use vike_model::accounts::account_keys::{AccountLabel, account_key};
use vike_oanda::{
    MountableTier, load_oanda_config_from, mountable_tier, mountable_tier_for_account,
    oanda_env_var_names,
};

#[test]
fn env_var_names_follow_the_oanda_prefix_scheme_per_tier() {
    assert_eq!(
        oanda_env_var_names(Environment::Sim),
        ("OANDA_SIM_API_KEY".to_string(), "OANDA_SIM_ACCOUNT_ID".to_string())
    );
    assert_eq!(
        oanda_env_var_names(Environment::Demo),
        ("OANDA_DEMO_API_KEY".to_string(), "OANDA_DEMO_ACCOUNT_ID".to_string())
    );
    assert_eq!(
        oanda_env_var_names(Environment::Live),
        ("OANDA_LIVE_API_KEY".to_string(), "OANDA_LIVE_ACCOUNT_ID".to_string())
    );
}

#[test]
fn live_falls_back_to_legacy_mainnet_tier_when_live_is_absent() {
    let mut vars = HashMap::new();
    vars.insert("OANDA_MAINNET_API_KEY".into(), "legacy-tok".into());
    vars.insert("OANDA_MAINNET_ACCOUNT_ID".into(), "101-001-0000001-001".into());

    let c =
        load_oanda_config_from(Environment::Live, &vars).expect("legacy MAINNET tier must load");
    assert_eq!(c.api_token, "legacy-tok");
    assert_eq!(c.account_id, "101-001-0000001-001");
    assert!(c.rest_base.contains("fxtrade"), "Live must resolve to the fxTrade hosts");
    assert!(c.stream_base.contains("fxtrade"));
}

#[test]
fn live_prefers_the_live_tier_over_legacy_mainnet_when_both_are_present() {
    let mut vars = HashMap::new();
    vars.insert("OANDA_MAINNET_API_KEY".into(), "legacy-tok".into());
    vars.insert("OANDA_MAINNET_ACCOUNT_ID".into(), "legacy-acct".into());
    vars.insert("OANDA_LIVE_API_KEY".into(), "new-tok".into());
    vars.insert("OANDA_LIVE_ACCOUNT_ID".into(), "new-acct".into());

    let c = load_oanda_config_from(Environment::Live, &vars).unwrap();
    assert_eq!(c.api_token, "new-tok");
    assert_eq!(c.account_id, "new-acct");
}

#[test]
fn demo_and_sim_have_no_legacy_fallback() {
    // Environment::legacy_str() only returns Some(_) for Live, so Demo/Sim get no MAINNET-style
    // fallback: a MAINNET-only .env must NOT satisfy a Demo or Sim request.
    let mut vars = HashMap::new();
    vars.insert("OANDA_MAINNET_API_KEY".into(), "tok".into());
    vars.insert("OANDA_MAINNET_ACCOUNT_ID".into(), "acct".into());
    assert!(load_oanda_config_from(Environment::Demo, &vars).is_none());
    assert!(load_oanda_config_from(Environment::Sim, &vars).is_none());
}

#[test]
fn partial_credentials_are_the_live_gate_either_field_missing_is_none() {
    let mut vars = HashMap::new();
    vars.insert("OANDA_DEMO_API_KEY".into(), "tok-only".into());
    assert!(
        load_oanda_config_from(Environment::Demo, &vars).is_none(),
        "api key without account id must not load"
    );

    let mut vars2 = HashMap::new();
    vars2.insert("OANDA_DEMO_ACCOUNT_ID".into(), "101-004-1234567-001".into());
    assert!(
        load_oanda_config_from(Environment::Demo, &vars2).is_none(),
        "account id without api key must not load"
    );
}

#[test]
fn blank_values_are_treated_as_absent() {
    let mut vars = HashMap::new();
    vars.insert("OANDA_DEMO_API_KEY".into(), "   ".into());
    vars.insert("OANDA_DEMO_ACCOUNT_ID".into(), "101-004-1234567-001".into());
    assert!(
        load_oanda_config_from(Environment::Demo, &vars).is_none(),
        "whitespace-only key is blank"
    );
}

#[test]
fn values_are_trimmed() {
    let mut vars = HashMap::new();
    vars.insert("OANDA_DEMO_API_KEY".into(), "  tok-abc  ".into());
    vars.insert("OANDA_DEMO_ACCOUNT_ID".into(), "  101-004-1234567-001  ".into());
    let c = load_oanda_config_from(Environment::Demo, &vars).unwrap();
    assert_eq!(c.api_token, "tok-abc");
    assert_eq!(c.account_id, "101-004-1234567-001");
}

// --- the live tier has no caller, so a live-named store is REFUSED --------------------------

/// A practice-only store resolves the one tier that has a caller.
#[test]
fn a_practice_only_store_resolves_the_practice_tier() {
    let mut vars = HashMap::new();
    vars.insert("OANDA_DEMO_API_KEY".into(), "tok".into());
    vars.insert("OANDA_DEMO_ACCOUNT_ID".into(), "101-004-1234567-001".into());
    match mountable_tier(&vars) {
        MountableTier::Practice(c) => assert!(c.rest_base.contains("fxpractice")),
        other => panic!("practice creds must resolve Practice, got {other:?}"),
    }
}

/// An empty store is the ORDINARY unconfigured state — absent credentials are the live gate, and
/// an absence is silent. This is the control that keeps the refusal below from being a blanket.
#[test]
fn an_empty_store_is_unconfigured_not_a_refusal() {
    assert!(matches!(mountable_tier(&HashMap::new()), MountableTier::Unconfigured));
}

/// **THE REFUSAL FIRES.** `OANDA_LIVE_*` names a tier `oanda_hosts` implements and no caller ever
/// asks for, so it must not resolve to *anything* — least of all silently to nothing.
#[test]
fn a_live_key_set_is_refused_and_names_the_variables_it_found() {
    let mut vars = HashMap::new();
    vars.insert("OANDA_LIVE_API_KEY".into(), "live-tok".into());
    vars.insert("OANDA_LIVE_ACCOUNT_ID".into(), "001-001-0000001-001".into());
    let MountableTier::LiveUnreachable(refusal) = mountable_tier(&vars) else {
        panic!("a live key set must be refused, never silently ignored");
    };
    assert_eq!(refusal.names, ["OANDA_LIVE_API_KEY", "OANDA_LIVE_ACCOUNT_ID"]);
    let msg = refusal.to_string();
    for needle in ["OANDA_LIVE_API_KEY", "OANDA_LIVE_ACCOUNT_ID", "OANDA_DEMO_API_KEY", "PAPER"] {
        assert!(msg.contains(needle), "the refusal must name {needle}: {msg}");
    }
}

/// **The remedy names the verb that works.** The refusal used to send an operator to
/// `<project>/settings/secrets.env` — a file a box migrated to the settings database no longer
/// reads, so following the sentence changed nothing and the venue stayed paper. `vike-cli secrets
/// set` writes whichever store answers (the value on stdin or `--from-env`, never argv), so the
/// sentence names it once per variable, names the verb that says which store that is, and says
/// plainly what the one thing it cannot do is.
#[test]
fn the_remedy_names_the_verb_that_works_and_never_the_file() {
    let mut vars = HashMap::new();
    vars.insert("OANDA_LIVE_API_KEY".into(), "tok-secret-abc".into());
    vars.insert("OANDA_LIVE_ACCOUNT_ID".into(), "001-001-0000001-001".into());
    let MountableTier::LiveUnreachable(refusal) = mountable_tier(&vars) else {
        panic!("expected a refusal");
    };
    let msg = refusal.to_string();
    let (key, account) = oanda_env_var_names(Environment::Demo);
    assert!(!msg.contains("secrets.env"), "names a file a migrated box does not read: {msg}");
    for name in [&key, &account] {
        assert!(
            msg.contains(&format!("vike-cli secrets set {name}")),
            "must name the verb that writes {name}: {msg}"
        );
    }
    assert!(
        msg.contains("vike-cli secrets list"),
        "must name the verb that says which store: {msg}"
    );
    assert!(msg.contains("never on the command line"), "a value never goes in argv: {msg}");
    // Narrowed to what is true of THESE rows: other verbs do remove other rows (`secrets
    // move-venue-config` deletes config-shaped ones, `secrets account deactivate` exists), but
    // nothing removes a practice or live-named oanda token row from a database store.
    assert!(
        msg.contains("no `secrets` verb removes these rows"),
        "must say that no verb removes THESE rows, so the sentence does not promise one: {msg}"
    );
    assert!(!msg.contains("deletes a credential"), "the over-broad claim is gone: {msg}");
    assert!(!msg.contains("tok-secret-abc"), "names only, never a value: {msg}");
}

/// **A LABELLED account's remedy names THAT account's practice keys.** The refusal is printed for a
/// labelled account too (`VenueMount::report_unmounted_account`), and `vike-cli secrets set` is an
/// UPSERT: a remedy that named the unlabelled `OANDA_DEMO_API_KEY` would tell the operator to
/// overwrite the DEFAULT account's practice token with the labelled account's.
#[test]
fn a_labelled_accounts_remedy_names_that_accounts_own_practice_keys() {
    let alt = AccountLabel::parse("ALT").expect("a legal label");
    let (live_key, _) = oanda_env_var_names(Environment::Live);
    let (demo_key, demo_account) = oanda_env_var_names(Environment::Demo);
    let mut vars = HashMap::new();
    vars.insert(account_key(&live_key, &alt), "tok-secret-abc".to_string());
    let MountableTier::LiveUnreachable(refusal) = mountable_tier_for_account(&alt, &vars) else {
        panic!("a labelled live-named key must be refused for that account");
    };
    let msg = refusal.to_string();
    for own in [account_key(&demo_key, &alt), account_key(&demo_account, &alt)] {
        assert!(
            msg.contains(&format!("vike-cli secrets set {own}`")),
            "must name the labelled account's own practice key `{own}`: {msg}"
        );
    }
    // The unlabelled name followed by the closing backtick is the DEFAULT account's key; the
    // labelled name merely CONTAINS it, so the backtick is what tells them apart.
    for default in [&demo_key, &demo_account] {
        assert!(
            !msg.contains(&format!("vike-cli secrets set {default}`")),
            "must not send the operator to overwrite the DEFAULT account's `{default}`: {msg}"
        );
    }
    assert!(!msg.contains("tok-secret-abc"), "names only, never a value: {msg}");

    // …and the DEFAULT account's own remedy is unchanged: its keys carry no label.
    let mut default_vars = HashMap::new();
    default_vars.insert(live_key, "tok-secret-abc".to_string());
    let MountableTier::LiveUnreachable(default_refusal) = mountable_tier(&default_vars) else {
        panic!("the default account's live-named key must be refused");
    };
    let default_msg = default_refusal.to_string();
    for default in [&demo_key, &demo_account] {
        assert!(
            default_msg.contains(&format!("vike-cli secrets set {default}`")),
            "the default account's remedy names its own `{default}`: {default_msg}"
        );
    }
}

/// The token is a bearer credential: it never reaches the finding, its `Debug`, or the rendered
/// line — the rule `OandaConfig`'s hand-written `Debug` keeps, applied to the refusal too.
#[test]
fn the_refusal_never_echoes_a_credential_value() {
    let mut vars = HashMap::new();
    vars.insert("OANDA_LIVE_API_KEY".into(), "tok-secret-abc".into());
    vars.insert("OANDA_LIVE_ACCOUNT_ID".into(), "001-001-0000001-001".into());
    let MountableTier::LiveUnreachable(refusal) = mountable_tier(&vars) else {
        panic!("expected a refusal");
    };
    assert!(!refusal.to_string().contains("tok-secret-abc"), "Display leaked the token");
    assert!(!format!("{refusal:?}").contains("tok-secret-abc"), "Debug leaked the token");
}

/// **The store this fix exists for.** Both tiers present used to mount REAL orders on the practice
/// account while the operator believed their live keys were in force. The live-named set wins, so
/// the practice fallback is refused with it.
#[test]
fn a_live_key_set_wins_over_a_present_practice_one() {
    let mut vars = HashMap::new();
    vars.insert("OANDA_DEMO_API_KEY".into(), "demo-tok".into());
    vars.insert("OANDA_DEMO_ACCOUNT_ID".into(), "101-004-1234567-001".into());
    vars.insert("OANDA_LIVE_API_KEY".into(), "live-tok".into());
    vars.insert("OANDA_LIVE_ACCOUNT_ID".into(), "001-001-0000001-001".into());
    assert!(
        matches!(mountable_tier(&vars), MountableTier::LiveUnreachable(_)),
        "a live-armed store must never fall through to the practice tier"
    );
}

/// The LEGACY `OANDA_MAINNET_*` pair is the same live tier under its pre-rename spelling
/// (`Environment::legacy_str`), and `load_oanda_config_from(Live, …)` still accepts it — so it is
/// refused identically. Missing this would leave the exact same silent-ignore behind an older
/// spelling still in use.
#[test]
fn the_legacy_mainnet_spelling_is_refused_too() {
    let mut vars = HashMap::new();
    vars.insert("OANDA_MAINNET_API_KEY".into(), "legacy-tok".into());
    vars.insert("OANDA_MAINNET_ACCOUNT_ID".into(), "001-001-0000001-001".into());
    let MountableTier::LiveUnreachable(refusal) = mountable_tier(&vars) else {
        panic!("the legacy MAINNET tier is the live tier and must be refused");
    };
    assert_eq!(refusal.names, ["OANDA_MAINNET_API_KEY", "OANDA_MAINNET_ACCOUNT_ID"]);
}

/// HALF a live pair is a half-written LIVE intent, not an absence. Reading it as "unconfigured"
/// hands back the same silent ignore in a smaller store.
#[test]
fn half_a_live_pair_is_still_refused() {
    let mut vars = HashMap::new();
    vars.insert("OANDA_LIVE_API_KEY".into(), "live-tok".into());
    let MountableTier::LiveUnreachable(refusal) = mountable_tier(&vars) else {
        panic!("half a live key set is still a live-armed store");
    };
    assert_eq!(refusal.names, ["OANDA_LIVE_API_KEY"]);
}

/// …but a BLANK one configures nothing and nobody can believe a tier is armed from it — the same
/// blank-is-absent rule `load_oanda_config_from` already applies. Without this, a commented-out
/// `OANDA_LIVE_API_KEY=` line left in a store would strand a working practice mount on paper.
#[test]
fn a_blank_live_variable_is_absent_not_a_refusal() {
    let mut vars = HashMap::new();
    vars.insert("OANDA_LIVE_API_KEY".into(), "   ".into());
    vars.insert("OANDA_DEMO_API_KEY".into(), "tok".into());
    vars.insert("OANDA_DEMO_ACCOUNT_ID".into(), "101-004-1234567-001".into());
    assert!(
        matches!(mountable_tier(&vars), MountableTier::Practice(_)),
        "a blank live variable arms nothing and must not refuse the practice tier"
    );
}
