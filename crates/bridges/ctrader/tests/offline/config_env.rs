//! Integration coverage for `CtraderConfig`'s credential gate. Field-driven and deterministic:
//! `from_credentials` is the unit under test, over an account's fields and the venue's application
//! fields built in the test (no process-env mutation, so nothing to serialize/race across tests —
//! `std::env::set_var` is process-global and NOT thread-safe to mutate from parallel `#[test]`
//! functions). `from_credentials` is also the ONLY entry point — the former `from_env` (a
//! whole-store self-sweep) was deleted when `CatalogProviderCtrader::new` moved to caller-supplied
//! credentials (the settings-registry convention: the caller owns the store read), and the
//! name-keyed `from_vars` with decision 0156 item 6 (a credential belongs to its account, read by
//! FIELD).

use vike_ctrader::config::{CtraderConfig, CtraderUrls, ctrader_host};
use vike_model::AccountTier;
use vike_secrets::account_credentials::{AccountCredentials, CredentialFields};
use vike_venues::venue_urls::VenueUrls;

/// The release seed's addresses at `tier` (`vike_venues::venue_urls::VENUE_URL_SEED`).
fn seeded(tier: AccountTier) -> CtraderUrls {
    ctrader_host(&VenueUrls::seed("ctrader"), tier).expect("the seed holds ctrader's rows")
}

/// The account's complete grant.
fn grant() -> CredentialFields {
    CredentialFields::from_pairs([
        ("ACCESS_TOKEN", "super-secret-access-token"),
        ("REFRESH_TOKEN", "super-secret-refresh-token"),
    ])
}

/// The venue's application pair.
fn app() -> CredentialFields {
    CredentialFields::from_pairs([
        ("CLIENT_ID", "cid-123"),
        ("CLIENT_SECRET", "super-secret-client-secret"),
    ])
}

fn load(
    tier: AccountTier,
    account: &CredentialFields,
    venue: &CredentialFields,
) -> Option<CtraderConfig> {
    CtraderConfig::from_credentials(
        &seeded(tier),
        &AccountCredentials::new(Some(1), Some(account), Some(venue)),
    )
}

#[test]
fn no_credentials_gate_to_none() {
    assert!(
        CtraderConfig::from_credentials(&seeded(AccountTier::Demo), &AccountCredentials::none())
            .is_none()
    );
    assert!(load(AccountTier::Demo, &CredentialFields::new(), &CredentialFields::new()).is_none());
}

#[test]
fn a_full_demo_grant_loads_with_the_demo_host_and_port() {
    let config = load(AccountTier::Demo, &grant(), &app()).expect("must load");
    let demo = seeded(AccountTier::Demo);
    assert_eq!(
        (config.host, config.port, config.oauth_base),
        (demo.host, demo.port, demo.oauth_base)
    );
    assert_eq!(config.account_id, None, "account id is optional — discovered at connect time");
}

#[test]
fn a_live_account_resolves_to_the_live_host() {
    let config = load(AccountTier::Live, &grant(), &app()).expect("must load");
    let live = seeded(AccountTier::Live);
    assert_eq!((&config.host, config.port), (&live.host, live.port));
    assert_ne!(config.host, seeded(AccountTier::Demo).host, "the live row, never the demo one");
}

#[test]
fn debug_never_leaks_the_client_secret_or_either_token() {
    let config = load(AccountTier::Demo, &grant(), &app()).unwrap();
    let debug = format!("{config:?}");
    assert!(!debug.contains("super-secret-client-secret"), "leaked client_secret: {debug}");
    assert!(!debug.contains("super-secret-access-token"), "leaked access_token: {debug}");
    assert!(!debug.contains("super-secret-refresh-token"), "leaked refresh_token: {debug}");
    assert!(debug.contains("cid-123"), "client_id should stay visible: {debug}");
    assert!(debug.contains(&seeded(AccountTier::Demo).host));
}

#[test]
fn each_required_field_missing_alone_gates_to_none() {
    for missing in ["ACCESS_TOKEN", "REFRESH_TOKEN"] {
        let mut account = CredentialFields::new();
        for field in ["ACCESS_TOKEN", "REFRESH_TOKEN"].into_iter().filter(|f| *f != missing) {
            account.insert(field, "v");
        }
        assert!(load(AccountTier::Demo, &account, &app()).is_none(), "missing {missing} must gate");
    }
    for missing in ["CLIENT_ID", "CLIENT_SECRET"] {
        let mut venue = CredentialFields::new();
        for field in ["CLIENT_ID", "CLIENT_SECRET"].into_iter().filter(|f| *f != missing) {
            venue.insert(field, "v");
        }
        assert!(load(AccountTier::Demo, &grant(), &venue).is_none(), "missing {missing} must gate");
    }
}

/// The application pair is the VENUE's: an account that holds `CLIENT_ID`/`CLIENT_SECRET` as its
/// OWN fields has not supplied them — the loader reads the venue's.
#[test]
fn the_app_pair_is_read_from_the_venue_not_the_account() {
    let mut account = grant();
    account.insert("CLIENT_ID", "cid");
    account.insert("CLIENT_SECRET", "sec");
    assert!(load(AccountTier::Demo, &account, &CredentialFields::new()).is_none());
}

#[test]
fn account_id_is_optional_but_parsed_when_present() {
    let mut account = grant();
    account.insert("ACCOUNT_ID", "42");
    let config = load(AccountTier::Demo, &account, &app()).unwrap();
    assert_eq!(config.account_id, Some(42));
}

#[test]
fn blank_values_are_treated_as_absent() {
    let mut venue = app();
    venue.insert("CLIENT_SECRET", "   ");
    assert!(load(AccountTier::Demo, &grant(), &venue).is_none());
}

#[test]
fn values_are_trimmed() {
    let mut venue = app();
    venue.insert("CLIENT_ID", "  cid-123  ");
    let config = load(AccountTier::Demo, &grant(), &venue).unwrap();
    assert_eq!(config.client_id, "cid-123");
}

#[test]
fn to_connection_config_carries_the_refresh_token_and_account_id() {
    let mut account = grant();
    account.insert("ACCOUNT_ID", "42");
    let config = load(AccountTier::Demo, &account, &app()).unwrap();
    let connection_config = config.to_connection_config();
    let demo = seeded(AccountTier::Demo);
    assert_eq!((connection_config.host, connection_config.port), (demo.host, demo.port));
    assert_eq!(
        connection_config.oauth_base, demo.oauth_base,
        "the refresh goes to the account's login row"
    );
    assert_eq!(connection_config.account_id, Some(42));
    assert_eq!(
        connection_config.refresh_token.as_ref().map(|t| t.expose().as_str()),
        Some("super-secret-refresh-token")
    );
    assert!(!connection_config.no_tls, "production config must use TLS");
}
