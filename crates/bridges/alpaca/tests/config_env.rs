//! The credential gate of `load_alpaca_config_from`: the account's three FIELDS (`CLIENT_ID` /
//! `CLIENT_SECRET` / `ACCOUNT_ID`) all required with a blank value counted as absent, the tier
//! picking only the hosts (sandbox below live, out of the release's `venue_url` rows), and the
//! client secret kept out of the config's `Debug`.

use vike_alpaca::{AlpacaConfig, load_alpaca_config_from};
use vike_model::AccountTier;
use vike_secrets::account_credentials::{AccountCredentials, CredentialFields};
use vike_venues::venue_urls::VenueUrls;

fn creds(fields: &CredentialFields) -> AccountCredentials<'_> {
    AccountCredentials::new(Some(1), Some(fields), None)
}

/// The loader on the release's seed rows, whose every host row is present.
fn load(tier: AccountTier, creds: &AccountCredentials<'_>) -> Option<AlpacaConfig> {
    load_alpaca_config_from(tier, creds, &VenueUrls::seed("alpaca"))
        .map(|c| c.expect("the seed holds every alpaca host row"))
}

#[test]
fn the_tier_picks_only_the_hosts() {
    let fields = CredentialFields::from_pairs([
        ("CLIENT_ID", "CID123"),
        ("CLIENT_SECRET", "SEC456"),
        ("ACCOUNT_ID", "acct-789"),
    ]);
    for tier in [AccountTier::Paper, AccountTier::Demo] {
        let c = load(tier, &creds(&fields)).expect("the whole trio loads");
        assert!(c.hosts.broker.contains("sandbox"), "{tier}");
    }
    let live = load(AccountTier::Live, &creds(&fields)).expect("loads");
    assert!(!live.hosts.broker.contains("sandbox"));
    assert_eq!(live.account_id, "acct-789", "the same fields at every tier");
}

/// A whole trio over rows that lack the sandbox's is the row's refusal, never another host.
#[test]
fn a_whole_trio_without_its_rows_is_refused_naming_the_row() {
    let fields = CredentialFields::from_pairs([
        ("CLIENT_ID", "CID123"),
        ("CLIENT_SECRET", "SEC456"),
        ("ACCOUNT_ID", "acct-789"),
    ]);
    let none = VenueUrls::from_rows("alpaca", std::iter::empty());
    let why = load_alpaca_config_from(AccountTier::Demo, &creds(&fields), &none)
        .expect("the trio is whole")
        .expect_err("no row");
    assert!(why.to_string().contains("no address row for demo login rest"), "{why}");
}

#[test]
fn gate_requires_all_three_fields() {
    let mut fields = CredentialFields::new();
    assert!(load(AccountTier::Demo, &creds(&fields)).is_none());
    fields.insert("CLIENT_ID", "CID123");
    // secret + account missing
    assert!(load(AccountTier::Demo, &creds(&fields)).is_none());
    fields.insert("CLIENT_SECRET", "SEC456");
    assert!(load(AccountTier::Demo, &creds(&fields)).is_none()); // account missing
    fields.insert("ACCOUNT_ID", "acct-789");
    let c = load(AccountTier::Demo, &creds(&fields)).unwrap();
    assert_eq!(c.client_id.expose(), "CID123");
    assert_eq!(c.account_id, "acct-789");
    assert!(c.hosts.broker.contains("sandbox"));
    assert!(load(AccountTier::Demo, &AccountCredentials::none()).is_none());
}

#[test]
fn secret_never_leaks_in_debug() {
    let fields = CredentialFields::from_pairs([
        ("CLIENT_ID", "CID123"),
        ("CLIENT_SECRET", "topsecret-DO-NOT-LEAK"),
        ("ACCOUNT_ID", "acct-789"),
    ]);
    let c = load(AccountTier::Demo, &creds(&fields)).unwrap();
    let dbg = format!("{c:?}");
    assert!(!dbg.contains("topsecret-DO-NOT-LEAK"), "secret leaked in Debug: {dbg}");
}

#[test]
fn blank_values_are_treated_as_absent() {
    let fields = CredentialFields::from_pairs([
        ("CLIENT_ID", "   "),
        ("CLIENT_SECRET", "SEC"),
        ("ACCOUNT_ID", "acct"),
    ]);
    assert!(load(AccountTier::Demo, &creds(&fields)).is_none());
}
