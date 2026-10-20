use super::*;
use vike_secrets::account_credentials::CredentialFields;
use vike_secrets::venue_setting::VenueSettings;
use vike_venues::credential_schema::{FieldOwner, schema_for};

use crate::recon_client::VENUE;

fn none() -> VenueSettings {
    VenueSettings::default()
}

/// The venue's `venue.fxcm.<tier>.<field>` rows — the shape the composition root reads out of
/// the settings database (decision 0095).
fn settings(tier: &str, rows: &[(&str, &str)]) -> VenueSettings {
    let rows: Vec<vike_secrets::VenueSettingRow> = rows
        .iter()
        .map(|(field, value)| vike_secrets::VenueSettingRow {
            venue: "fxcm".to_string(),
            tier: Some(tier.parse().expect("a tier")),
            field: field.to_ascii_uppercase(),
            value: (*value).to_string(),
        })
        .collect();
    VenueSettings::from_rows("fxcm", &rows)
}

/// An account holding `pairs` (`(FIELD, value)`).
fn fields(pairs: &[(&str, &str)]) -> CredentialFields {
    CredentialFields::from_pairs(pairs.iter().copied())
}

fn creds(fields: &CredentialFields) -> AccountCredentials<'_> {
    AccountCredentials::new(Some(1), Some(fields), None)
}

#[test]
fn gate_defaults_and_no_password_leak() {
    // no creds -> live gate returns None
    assert!(
        load_fxcm_config_from(AccountTier::Demo, &AccountCredentials::none(), &none()).is_none()
    );

    let login = fields(&[("USER", "D251112911"), ("PASSWORD", "s3cr3t")]);
    let c = load_fxcm_config_from(AccountTier::Demo, &creds(&login), &none()).unwrap();
    assert_eq!(c.user, "D251112911");
    assert_eq!(c.connection, "Demo"); // default for Demo
    assert!(c.url.contains("fxcorporate"));
    // Debug must not leak the password
    assert!(!format!("{c:?}").contains("s3cr3t"));

    // The same fields at the live tier read the live default connection: the tier picks the
    // settings, never the credential.
    let c2 = load_fxcm_config_from(AccountTier::Live, &creds(&login), &none()).unwrap();
    assert_eq!(c2.connection, "Real"); // default for Live
    let c3 = load_fxcm_config_from(AccountTier::Paper, &creds(&login), &none()).unwrap();
    assert_eq!(c3.connection, "Demo"); // `paper` reads as `demo`

    // Half a login is no login, and a blank field is absent.
    let halves: [&[(&str, &str)]; 3] =
        [&[("USER", "u")], &[("PASSWORD", "p")], &[("USER", " "), ("PASSWORD", "p")]];
    for half in halves {
        assert!(load_fxcm_config_from(AccountTier::Demo, &creds(&fields(half)), &none()).is_none());
    }
}

/// Decision 0095, Task 7: the host URL and the connection name are the TIER's `venue_setting`
/// rows, read at the tier the loader is asked for.
#[test]
fn url_and_connection_come_from_the_tiers_settings() {
    let login = fields(&[("USER", "u"), ("PASSWORD", "p")]);
    let s =
        settings("demo", &[("url", "http://elsewhere.example/Hosts.jsp"), ("connection", "Real")]);
    let c = load_fxcm_config_from(AccountTier::Demo, &creds(&login), &s).unwrap();
    assert_eq!(
        (c.url.as_str(), c.connection.as_str()),
        ("http://elsewhere.example/Hosts.jsp", "Real")
    );

    // A credential FIELD named like a setting is read by nothing: the connection is a setting.
    let with_connection_field = fields(&[("USER", "u"), ("PASSWORD", "p"), ("CONNECTION", "Real")]);
    let ignored =
        load_fxcm_config_from(AccountTier::Demo, &creds(&with_connection_field), &none()).unwrap();
    assert_eq!(ignored.connection, "Demo", "a credential-field connection is read by nothing");

    // The LIVE tier reads the LIVE tier's rows.
    let real = settings("live", &[("connection", "Demo")]);
    assert_eq!(
        load_fxcm_config_from(AccountTier::Live, &creds(&login), &real).unwrap().connection,
        "Demo"
    );

    // A padded row reads trimmed and a whitespace-only one reads as no row — `vike-cli config
    // set` and the move copy a value as written.
    let padded = settings(
        "demo",
        &[("url", " http://elsewhere.example/Hosts.jsp "), ("connection", " Real ")],
    );
    let c = load_fxcm_config_from(AccountTier::Demo, &creds(&login), &padded).unwrap();
    assert_eq!(
        (c.url.as_str(), c.connection.as_str()),
        ("http://elsewhere.example/Hosts.jsp", "Real")
    );
    let blank = settings("demo", &[("url", "   "), ("connection", " ")]);
    let c = load_fxcm_config_from(AccountTier::Demo, &creds(&login), &blank).unwrap();
    assert_eq!((c.url.as_str(), c.connection.as_str()), (DEFAULT_HOST_URL, "Demo"));

    // The two fields are TIER-scoped: a machine-scoped (`any`) row is not this tier's value.
    let any = VenueSettings::from_rows(
        "fxcm",
        &[vike_secrets::VenueSettingRow {
            venue: "fxcm".to_string(),
            tier: None,
            field: "CONNECTION".to_string(),
            value: "Real".to_string(),
        }],
    );
    assert_eq!(
        load_fxcm_config_from(AccountTier::Demo, &creds(&login), &any).unwrap().connection,
        "Demo"
    );
}

/// **The fields this file reads are `vike_venues::credential_schema`'s**: FXCM's row holds exactly
/// [`FIELDS`] for an account, both required, and nothing for the venue.
#[test]
fn the_fields_are_the_schemas() {
    let schema = schema_for(VENUE).expect("fxcm is a schema row");
    let account: Vec<&str> = schema.account_fields().map(|f| f.field).collect();
    assert_eq!(account, FIELDS);
    assert_eq!(schema.required(FieldOwner::Account), FIELDS);
    assert_eq!(schema.venue_fields().count(), 0);
}

/// The arm's one store-side decision: a complete login arms the DEMO login below `live` (on the
/// DEMO tier's settings, whatever the account's tier), is refused at `live`, and anything less is
/// unconfigured at every tier.
#[test]
fn the_demo_login_follows_the_accounts_one_tier() {
    let full = fields(&[("USER", "u"), ("PASSWORD", "p")]);
    let half = fields(&[("USER", "u")]);
    let demo_rows = settings("demo", &[("connection", "Real")]);
    for tier in [AccountTier::Paper, AccountTier::Demo] {
        let DemoLogin::Demo(cfg) = demo_login(tier, &creds(&full), &demo_rows) else {
            panic!("a complete login below live is the demo login ({tier})");
        };
        assert_eq!(cfg.connection, "Real", "the DEMO tier's settings ({tier})");
    }
    assert!(matches!(
        demo_login(AccountTier::Live, &creds(&full), &none()),
        DemoLogin::LiveTierNotWired
    ));
    for tier in AccountTier::ALL {
        assert!(matches!(demo_login(tier, &creds(&half), &none()), DemoLogin::Unconfigured));
        assert!(matches!(
            demo_login(tier, &AccountCredentials::none(), &none()),
            DemoLogin::Unconfigured
        ));
    }
}
