//! Integration coverage for OANDA's bespoke credential shape (`load_oanda_config_from`) — the
//! account's bearer-token + account-id FIELDS (`API_KEY` / `ACCOUNT_ID`), NOT the HMAC
//! `Credentials` shape every other venue uses. The inline unit test in `config.rs` only exercises
//! the practice hosts; this adds the live hosts (the same fields at every tier — no name carries a
//! tier, decision 0156 item 6), the partial-credential gate, and `mountable_tier` — the refusal a
//! LIVE account's pair earns, since this workspace can reach no fxTrade tier at all.

use std::assert_matches;

use vike_model::AccountTier;
use vike_model::accounts::account_keys::AccountLabel;
use vike_oanda::{MountableTier, load_oanda_config_from, mountable_tier};
use vike_secrets::account_credentials::{AccountCredentials, CredentialFields};

fn fields(pairs: &[(&str, &str)]) -> CredentialFields {
    CredentialFields::from_pairs(pairs.iter().copied())
}

fn creds(fields: &CredentialFields) -> AccountCredentials<'_> {
    AccountCredentials::new(Some(1), Some(fields), None)
}

/// The default account's tier decision.
fn tier_of(tier: AccountTier, fields: &CredentialFields) -> MountableTier {
    mountable_tier(&AccountLabel::Default, tier, &creds(fields))
}

#[test]
fn the_same_fields_load_at_every_tier_and_the_tier_picks_the_hosts() {
    let pair = fields(&[("API_KEY", "new-tok"), ("ACCOUNT_ID", "new-acct")]);

    let c = load_oanda_config_from(AccountTier::Live, &creds(&pair)).expect("the pair must load");
    assert_eq!(c.api_token, "new-tok");
    assert_eq!(c.account_id, "new-acct");
    assert!(c.rest_base.contains("fxtrade"), "Live must resolve to the fxTrade hosts");
    assert!(c.stream_base.contains("fxtrade"));

    for tier in [AccountTier::Paper, AccountTier::Demo] {
        let c = load_oanda_config_from(tier, &creds(&pair)).expect("the pair must load");
        assert!(c.rest_base.contains("fxpractice"), "{tier}");
        assert!(c.stream_base.contains("fxpractice"), "{tier}");
    }
}

#[test]
fn partial_credentials_are_the_live_gate_either_field_missing_is_none() {
    assert!(
        load_oanda_config_from(AccountTier::Demo, &creds(&fields(&[("API_KEY", "tok-only")])))
            .is_none(),
        "api key without account id must not load"
    );
    assert!(
        load_oanda_config_from(
            AccountTier::Demo,
            &creds(&fields(&[("ACCOUNT_ID", "101-004-1234567-001")]))
        )
        .is_none(),
        "account id without api key must not load"
    );
    assert!(load_oanda_config_from(AccountTier::Demo, &AccountCredentials::none()).is_none());
}

#[test]
fn blank_values_are_treated_as_absent() {
    let pair = fields(&[("API_KEY", "   "), ("ACCOUNT_ID", "101-004-1234567-001")]);
    assert!(
        load_oanda_config_from(AccountTier::Demo, &creds(&pair)).is_none(),
        "whitespace-only key is blank"
    );
}

#[test]
fn values_are_trimmed() {
    let pair = fields(&[("API_KEY", "  tok-abc  "), ("ACCOUNT_ID", "  101-004-1234567-001  ")]);
    let c = load_oanda_config_from(AccountTier::Demo, &creds(&pair)).unwrap();
    assert_eq!(c.api_token, "tok-abc");
    assert_eq!(c.account_id, "101-004-1234567-001");
}

// --- the live tier has no caller, so a LIVE account's pair is REFUSED -------------------------

/// A practice account resolves the one tier that has a caller — and a `paper` tier reads as the
/// demo one (the mount contract's rule).
#[test]
fn a_practice_account_resolves_the_practice_tier() {
    let pair = fields(&[("API_KEY", "tok"), ("ACCOUNT_ID", "101-004-1234567-001")]);
    for tier in [AccountTier::Paper, AccountTier::Demo] {
        match tier_of(tier, &pair) {
            MountableTier::Practice(c) => assert!(c.rest_base.contains("fxpractice")),
            other => panic!("practice creds must resolve Practice at {tier}, got {other:?}"),
        }
    }
}

/// An empty account is the ORDINARY unconfigured state — absent credentials are the live gate, and
/// an absence is silent. This is the control that keeps the refusal below from being a blanket.
#[test]
fn an_empty_account_is_unconfigured_not_a_refusal() {
    for tier in AccountTier::ALL {
        assert_matches!(tier_of(tier, &CredentialFields::new()), MountableTier::Unconfigured);
    }
}

/// **THE REFUSAL FIRES.** A `live` account's pair names a tier `oanda_hosts` implements and no
/// caller ever asks for, so it must not resolve to *anything* — least of all silently to nothing,
/// and never to the practice hosts.
#[test]
fn a_live_account_is_refused_and_names_the_account() {
    let pair = fields(&[("API_KEY", "live-tok"), ("ACCOUNT_ID", "001-001-0000001-001")]);
    let MountableTier::LiveUnreachable(refusal) = tier_of(AccountTier::Live, &pair) else {
        panic!("a live account's pair must be refused, never silently ignored");
    };
    assert_eq!(refusal.account, AccountLabel::Default);
    let msg = refusal.to_string();
    for needle in ["the default account", "LIVE", "PAPER", "fxTrade"] {
        assert!(msg.contains(needle), "the refusal must say {needle}: {msg}");
    }
}

/// **The remedy names what works**, never a file path: the practice account is its own account row
/// at tier `demo`, and `vike-cli secrets accounts` says what the store holds. No credential NAME is
/// named — a credential belongs to its account, and no name carries a tier.
#[test]
fn the_remedy_names_what_works_and_never_the_file() {
    let pair = fields(&[("API_KEY", "tok-secret-abc"), ("ACCOUNT_ID", "001-001-0000001-001")]);
    let MountableTier::LiveUnreachable(refusal) = tier_of(AccountTier::Live, &pair) else {
        panic!("expected a refusal");
    };
    let msg = refusal.to_string();
    assert!(!msg.contains("secrets.env"), "names a file a migrated box does not read: {msg}");
    assert!(msg.contains("tier `demo`"), "the practice account is a demo-tier row: {msg}");
    assert!(msg.contains("vike-cli secrets accounts"), "must name the verb that says what is held");
    assert!(!msg.contains("OANDA_"), "no composed credential name: {msg}");
    assert!(!msg.contains("tok-secret-abc"), "never a value: {msg}");
}

/// **A LABELLED account's refusal names THAT account**, and the default account's names the
/// default one: the refusal is printed for a labelled account too
/// (`VenueMount::report_unmounted_account`), so a sentence naming the wrong account would send the
/// operator to the wrong row.
#[test]
fn a_labelled_accounts_refusal_names_that_account() {
    let alt = AccountLabel::parse("ALT").expect("a legal label");
    let pair = fields(&[("API_KEY", "tok-secret-abc"), ("ACCOUNT_ID", "001-001-0000001-001")]);
    let MountableTier::LiveUnreachable(refusal) =
        mountable_tier(&alt, AccountTier::Live, &creds(&pair))
    else {
        panic!("a labelled live account's pair must be refused for that account");
    };
    let msg = refusal.to_string();
    assert!(msg.contains("account ALT"), "{msg}");
    assert!(!msg.contains("the default account"), "{msg}");
    assert!(!msg.contains("tok-secret-abc"), "never a value: {msg}");
}

/// The token is a bearer credential: it never reaches the finding, its `Debug`, or the rendered
/// line — the rule `OandaConfig`'s hand-written `Debug` keeps, applied to the refusal too.
#[test]
fn the_refusal_never_echoes_a_credential_value() {
    let pair = fields(&[("API_KEY", "tok-secret-abc"), ("ACCOUNT_ID", "001-001-0000001-001")]);
    let MountableTier::LiveUnreachable(refusal) = tier_of(AccountTier::Live, &pair) else {
        panic!("expected a refusal");
    };
    assert!(!refusal.to_string().contains("tok-secret-abc"), "Display leaked the token");
    assert!(!format!("{refusal:?}").contains("tok-secret-abc"), "Debug leaked the token");
    assert!(!format!("{refusal:?}").contains("001-001-0000001-001"), "Debug leaked the account id");
}

/// HALF a pair on a `live` account is what the loader calls absent: unconfigured, like half a pair
/// at any tier — an account has one tier, and a half-written set is no set.
#[test]
fn half_a_live_pair_is_unconfigured() {
    for half in [[("API_KEY", "live-tok")], [("ACCOUNT_ID", "001-001-0000001-001")]] {
        assert_matches!(tier_of(AccountTier::Live, &fields(&half)), MountableTier::Unconfigured);
    }
}

/// …and a BLANK field configures nothing, so nobody can believe a tier is armed from it — the same
/// blank-is-absent rule `load_oanda_config_from` applies.
#[test]
fn a_blank_live_field_is_absent_not_a_refusal() {
    let pair = fields(&[("API_KEY", "   "), ("ACCOUNT_ID", "001-001-0000001-001")]);
    assert_matches!(tier_of(AccountTier::Live, &pair), MountableTier::Unconfigured);
}
