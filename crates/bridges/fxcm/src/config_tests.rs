use super::*;
use vike_secrets::account_credentials::CredentialFields;
use vike_secrets::credentials::assert_account_fields_are_the_schemas;
use vike_secrets::venue_setting::VenueSettings;
use vike_venues::venue_urls::VenueUrls;

use crate::reconcile::VENUE;

/// The release's addresses, as a box with a fresh `venue_url` table holds them.
fn seeded() -> VenueSettings {
    VenueSettings::default().with_urls(VenueUrls::seed(VENUE))
}

/// The seed's `fxcm <ty> login rest <product>` url.
fn seed_login(ty: UrlType, product: &str) -> &'static str {
    vike_venues::venue_urls::seed_url(VENUE, ty, UrlChannel::Login, UrlProtocol::Rest, product)
        .expect("a seed login row")
}

/// An account holding `pairs` (`(FIELD, value)`).
fn fields(pairs: &[(&str, &str)]) -> CredentialFields {
    CredentialFields::from_pairs(pairs.iter().copied())
}

fn creds(fields: &CredentialFields) -> AccountCredentials<'_> {
    AccountCredentials::new(Some(1), Some(fields), None)
}

/// The whole login at `env` on `settings`, which must find its address row.
fn loaded(env: AccountTier, login: &CredentialFields, settings: &VenueSettings) -> FxcmConfig {
    load_fxcm_config_from(env, &creds(login), settings)
        .expect("a whole login")
        .expect("the tier's login row")
}

#[test]
fn gate_defaults_and_no_password_leak() {
    // no creds -> live gate returns None
    assert!(
        load_fxcm_config_from(AccountTier::Demo, &AccountCredentials::none(), &seeded()).is_none()
    );

    let login = fields(&[("USER", "D251112911"), ("PASSWORD", "s3cr3t")]);
    let c = loaded(AccountTier::Demo, &login, &seeded());
    assert_eq!(c.user, "D251112911");
    assert_eq!(
        (c.url.as_str(), c.connection.as_str()),
        (seed_login(UrlType::Demo, "Demo"), "Demo")
    );
    // Debug must not leak the password
    assert!(!format!("{c:?}").contains("s3cr3t"));

    // The same fields at the live tier read the live row on the `Real` connection: the tier picks
    // the address, never the credential.
    let c2 = loaded(AccountTier::Live, &login, &seeded());
    assert_eq!(
        (c2.url.as_str(), c2.connection.as_str()),
        (seed_login(UrlType::Live, "Real"), "Real")
    );
    let c3 = loaded(AccountTier::Paper, &login, &seeded());
    assert_eq!(c3.connection, "Demo"); // `paper` reads as `demo`

    // Half a login is no login, and a blank field is absent.
    let halves: [&[(&str, &str)]; 3] =
        [&[("USER", "u")], &[("PASSWORD", "p")], &[("USER", " "), ("PASSWORD", "p")]];
    for half in halves {
        assert!(
            load_fxcm_config_from(AccountTier::Demo, &creds(&fields(half)), &seeded()).is_none()
        );
    }
}

/// The host URL is the TIER's `fxcm <type> login rest <connection>` address row — an operator's
/// row when one is written — and the connection is that row's product: nothing else moves either.
#[test]
fn the_url_is_the_tiers_login_row_and_the_connection_its_product() {
    let login = fields(&[("USER", "u"), ("PASSWORD", "p")]);
    let row = |ty, product, url| {
        VenueSettings::default().with_urls(VenueUrls::from_rows(
            VENUE,
            [(VENUE, ty, UrlChannel::Login, UrlProtocol::Rest, product, url)],
        ))
    };
    let elsewhere = row(UrlType::Demo, "Demo", "http://elsewhere.example/Hosts.jsp");
    let c = loaded(AccountTier::Demo, &login, &elsewhere);
    assert_eq!(
        (c.url.as_str(), c.connection.as_str()),
        ("http://elsewhere.example/Hosts.jsp", "Demo")
    );

    // A credential FIELD named like the connection is read by nothing.
    let with_connection_field = fields(&[("USER", "u"), ("PASSWORD", "p"), ("CONNECTION", "Real")]);
    assert_eq!(loaded(AccountTier::Demo, &with_connection_field, &seeded()).connection, "Demo");

    // A box still holding the RETIRED `venue.fxcm.<tier>.{url,connection}` rows: read by nothing.
    let retired = vike_bridge_core::venue_adapter_fixture::venue_settings(
        "fxcm",
        "demo",
        &[("url", "http://retired.example/Hosts.jsp"), ("connection", "Real")],
    )
    .with_urls(VenueUrls::seed(VENUE));
    let c = loaded(AccountTier::Demo, &login, &retired);
    assert_eq!(
        (c.url.as_str(), c.connection.as_str()),
        (seed_login(UrlType::Demo, "Demo"), "Demo")
    );

    // A demo row is never the live tier's, and a row of the other connection is not this tier's:
    // no row, no login — the refusal names the row it looked for.
    let why = load_fxcm_config_from(AccountTier::Live, &creds(&login), &elsewhere)
        .expect("a whole login")
        .expect_err("the live tier has no row here");
    assert!(why.to_string().contains("live login rest product=Real"), "{why}");
    let real_on_demo = row(UrlType::Demo, "Real", "http://elsewhere.example/Hosts.jsp");
    assert!(
        load_fxcm_config_from(AccountTier::Demo, &creds(&login), &real_on_demo)
            .expect("a whole login")
            .is_err()
    );
}

/// **The fields this file reads are `vike_venues::credential_schema`'s**: FXCM's row holds exactly
/// [`FIELDS`] for an account, both required, and nothing for the venue.
#[test]
fn the_fields_are_the_schemas() {
    assert_account_fields_are_the_schemas(VENUE, &FIELDS);
}

/// The arm's one store-side decision: a complete login arms the DEMO login below `live` (on the
/// DEMO tier's address row, whatever the account's tier — a missing row still arms and rides as
/// the refusal), is refused at `live`, and anything less is unconfigured at every tier.
#[test]
fn the_demo_login_follows_the_accounts_one_tier() {
    let full = fields(&[("USER", "u"), ("PASSWORD", "p")]);
    let half = fields(&[("USER", "u")]);
    let none = VenueSettings::default();
    for tier in [AccountTier::Paper, AccountTier::Demo] {
        let ArmingDemoOnly::Demo(Ok(config)) = demo_login(tier, &creds(&full), &seeded()) else {
            panic!("a complete login below live is the demo login ({tier})");
        };
        assert_eq!(config.url, seed_login(UrlType::Demo, "Demo"), "the DEMO tier's row ({tier})");
        assert!(matches!(demo_login(tier, &creds(&full), &none), ArmingDemoOnly::Demo(Err(_))));
    }
    assert!(matches!(
        demo_login(AccountTier::Live, &creds(&full), &seeded()),
        ArmingDemoOnly::LiveTierNotWired
    ));
    for tier in AccountTier::ALL {
        assert!(matches!(demo_login(tier, &creds(&half), &seeded()), ArmingDemoOnly::Unconfigured));
        assert!(matches!(
            demo_login(tier, &AccountCredentials::none(), &seeded()),
            ArmingDemoOnly::Unconfigured
        ));
    }
}
