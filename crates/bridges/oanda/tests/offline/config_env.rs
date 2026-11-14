//! Integration coverage for OANDA's bespoke credential shape (`load_oanda_config_from`) — the
//! account's bearer-token + account-id FIELDS (`API_KEY` / `ACCOUNT_ID`), NOT the HMAC
//! `Credentials` shape every other venue uses. The inline unit test in `config.rs` only exercises
//! the practice hosts; this adds the live hosts (the same fields at every tier — no name carries a
//! tier, decision 0156 item 6), the partial-credential gate, and `demo_only_arm` — the refusal a
//! LIVE account's pair earns, since this workspace can reach no fxTrade tier at all.

use vike_bridge_core::venue_adapter::ArmingDemoOnly;
use vike_model::AccountTier;
use vike_model::accounts::account_keys::AccountLabel;
use vike_oanda::{OandaConfig, demo_only_arm, load_oanda_config_from};
use vike_secrets::account_credentials::{AccountCredentials, CredentialFields};
use vike_venues::venue_urls::{UrlRefusal, VenueUrls};

fn fields(pairs: &[(&str, &str)]) -> CredentialFields {
    CredentialFields::from_pairs(pairs.iter().copied())
}

fn creds(fields: &CredentialFields) -> AccountCredentials<'_> {
    AccountCredentials::new(Some(1), Some(fields), None)
}

/// [`load_oanda_config_from`] over the release's addresses (the seed's oanda rows).
fn load(tier: AccountTier, creds: &AccountCredentials<'_>) -> Option<OandaConfig> {
    load_oanda_config_from(tier, creds, &VenueUrls::seed("oanda")).expect("the seed's rows")
}

/// The tier decision of an account at `tier` holding `fields`.
fn tier_of(
    tier: AccountTier,
    fields: &CredentialFields,
) -> ArmingDemoOnly<Result<OandaConfig, UrlRefusal>> {
    demo_only_arm(tier, &creds(fields), &VenueUrls::seed("oanda"))
}

/// Whether an account at `tier` holding `fields` is the ordinary unconfigured state.
fn unconfigured(tier: AccountTier, fields: &CredentialFields) -> bool {
    matches!(tier_of(tier, fields), ArmingDemoOnly::Unconfigured)
}

/// The refusal `start` says for `account` when its `live` tier holds `fields` (the arm's own report, the
/// shared demo-only phrase in OANDA's word): it must be refused first, and said once.
fn refusal_of(account: AccountLabel, fields: &CredentialFields) -> String {
    let arm = tier_of(AccountTier::Live, fields);
    assert!(
        matches!(arm, ArmingDemoOnly::LiveTierNotWired),
        "a live account's pair must be refused, never silently ignored"
    );
    let ((), said) = vike_log::capture::captured(|| arm.report_live_tier("oanda", &account));
    assert_eq!(said.len(), 1, "{said:?}");
    said[0].message.clone()
}

#[test]
fn the_same_fields_load_at_every_tier_and_the_tier_picks_the_hosts() {
    let pair = fields(&[("API_KEY", "new-tok"), ("ACCOUNT_ID", "new-acct")]);

    let c = load(AccountTier::Live, &creds(&pair)).expect("the pair must load");
    assert_eq!(c.api_token.expose(), "new-tok");
    assert_eq!(c.account_id, "new-acct");
    assert!(c.rest_base.contains("fxtrade"), "Live must resolve to the fxTrade hosts");
    assert!(c.stream_base.contains("fxtrade"));

    for tier in [AccountTier::Paper, AccountTier::Demo] {
        let c = load(tier, &creds(&pair)).expect("the pair must load");
        assert!(c.rest_base.contains("fxpractice"), "{tier}");
        assert!(c.stream_base.contains("fxpractice"), "{tier}");
    }
}

#[test]
fn partial_credentials_are_the_live_gate_either_field_missing_is_none() {
    assert!(
        load(AccountTier::Demo, &creds(&fields(&[("API_KEY", "tok-only")]))).is_none(),
        "api key without account id must not load"
    );
    assert!(
        load(AccountTier::Demo, &creds(&fields(&[("ACCOUNT_ID", "101-004-1234567-001")])))
            .is_none(),
        "account id without api key must not load"
    );
    assert!(load(AccountTier::Demo, &AccountCredentials::none()).is_none());
}

#[test]
fn blank_values_are_treated_as_absent() {
    let pair = fields(&[("API_KEY", "   "), ("ACCOUNT_ID", "101-004-1234567-001")]);
    assert!(load(AccountTier::Demo, &creds(&pair)).is_none(), "whitespace-only key is blank");
}

#[test]
fn values_are_trimmed() {
    let pair = fields(&[("API_KEY", "  tok-abc  "), ("ACCOUNT_ID", "  101-004-1234567-001  ")]);
    let c = load(AccountTier::Demo, &creds(&pair)).unwrap();
    assert_eq!(c.api_token.expose(), "tok-abc");
    assert_eq!(c.account_id, "101-004-1234567-001");
}

// --- the live tier has no caller, so a LIVE account's pair is REFUSED -------------------------

/// A practice account resolves the one tier that has a caller — and a `paper` tier reads as the
/// demo one (the start contract's rule).
#[test]
fn a_practice_account_resolves_the_practice_tier() {
    let pair = fields(&[("API_KEY", "tok"), ("ACCOUNT_ID", "101-004-1234567-001")]);
    for tier in [AccountTier::Paper, AccountTier::Demo] {
        match tier_of(tier, &pair) {
            ArmingDemoOnly::Demo(Ok(c)) => assert!(c.rest_base.contains("fxpractice")),
            _ => panic!("practice creds must resolve the practice tier at {tier}"),
        }
    }
}

/// An empty account is the ORDINARY unconfigured state — absent credentials are the live gate, and
/// an absence is silent. This is the control that keeps the refusal below from being a blanket.
#[test]
fn an_empty_account_is_unconfigured_not_a_refusal() {
    for tier in AccountTier::ALL {
        assert!(unconfigured(tier, &CredentialFields::new()), "{tier}");
    }
}

/// **THE REFUSAL FIRES.** A `live` account's pair names a tier `oanda_hosts` implements and no
/// caller ever asks for, so it must not resolve to *anything* — least of all silently to nothing,
/// and never to the practice hosts.
#[test]
fn a_live_account_is_refused_and_names_the_account() {
    let pair = fields(&[("API_KEY", "live-tok"), ("ACCOUNT_ID", "001-001-0000001-001")]);
    let msg = refusal_of(AccountLabel::Default, &pair);
    // The shared demo-only phrase in OANDA's own word for the tier it does start (`practice`).
    for needle in ["the default account", "LIVE", "NOT RUNNING", "practice"] {
        assert!(msg.contains(needle), "the refusal must say {needle}: {msg}");
    }
}

/// **The remedy names what works**, never a file path: the practice account is its own account row
/// at tier `demo`, and `vike-cli secrets accounts` says what the store holds. No credential NAME is
/// named — a credential belongs to its account, and no name carries a tier.
#[test]
fn the_remedy_names_what_works_and_never_the_file() {
    let pair = fields(&[("API_KEY", "tok-secret-abc"), ("ACCOUNT_ID", "001-001-0000001-001")]);
    let msg = refusal_of(AccountLabel::Default, &pair);
    assert!(!msg.contains("secrets.env"), "names a file a migrated box does not read: {msg}");
    assert!(msg.contains("tier `demo`"), "the practice account is a demo-tier row: {msg}");
    assert!(msg.contains("vike-cli secrets accounts"), "must name the verb that says what is held");
    assert!(!msg.contains("OANDA_"), "no composed credential name: {msg}");
    assert!(!msg.contains("tok-secret-abc"), "never a value: {msg}");
}

/// **A LABELLED account's refusal names THAT account**, and the default account's names the
/// default one: the refusal is printed for a labelled account too
/// (`VenueAdapter::report_not_running_account`), so a sentence naming the wrong account would send the
/// operator to the wrong row.
#[test]
fn a_labelled_accounts_refusal_names_that_account() {
    let alt = AccountLabel::parse("ALT").expect("a legal label");
    let pair = fields(&[("API_KEY", "tok-secret-abc"), ("ACCOUNT_ID", "001-001-0000001-001")]);
    let msg = refusal_of(alt, &pair);
    assert!(msg.contains("account ALT"), "{msg}");
    assert!(!msg.contains("the default account"), "{msg}");
    assert!(!msg.contains("tok-secret-abc"), "never a value: {msg}");
}

/// The token is a bearer credential: it never reaches the refusal's line, and neither does the account
/// id — the rule `OandaConfig`'s hand-written `Debug` keeps, applied to the refusal too.
#[test]
fn the_refusal_never_echoes_a_credential_value() {
    let pair = fields(&[("API_KEY", "tok-secret-abc"), ("ACCOUNT_ID", "001-001-0000001-001")]);
    let refusal = refusal_of(AccountLabel::Default, &pair);
    assert!(!refusal.contains("tok-secret-abc"), "the line leaked the token: {refusal}");
    assert!(!refusal.contains("001-001-0000001-001"), "the line leaked the account id: {refusal}");
}

/// HALF a pair on a `live` account is what the loader calls absent: unconfigured, like half a pair
/// at any tier — an account has one tier, and a half-written set is no set.
#[test]
fn half_a_live_pair_is_unconfigured() {
    for half in [[("API_KEY", "live-tok")], [("ACCOUNT_ID", "001-001-0000001-001")]] {
        assert!(unconfigured(AccountTier::Live, &fields(&half)));
    }
}

/// …and a BLANK field configures nothing, so nobody can believe a tier is armed from it — the same
/// blank-is-absent rule `load_oanda_config_from` applies.
#[test]
fn a_blank_live_field_is_absent_not_a_refusal() {
    let pair = fields(&[("API_KEY", "   "), ("ACCOUNT_ID", "001-001-0000001-001")]);
    assert!(unconfigured(AccountTier::Live, &pair));
}
