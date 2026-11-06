//! The per-venue passphrase rule over ONE account's fields: OKX's signer sends a passphrase on every
//! request, so an OKX account holding key + secret and no passphrase is ABSENT, and the mount names
//! the missing FIELD.

use super::*;
use crate::account_credentials::CredentialFields;

fn view(fields: &CredentialFields) -> AccountCredentials<'_> {
    AccountCredentials::new(Some(1), Some(fields), None)
}

/// OKX with key + secret and no passphrase is ABSENT (the live gate), and the finding names the
/// passphrase FIELD — a half-configured account is not a configured one.
#[test]
fn okx_without_its_passphrase_is_absent_and_names_the_field() {
    let half = CredentialFields::from_pairs([(API_KEY, "k"), (API_SECRET, "s")]);
    assert!(api_credentials(&view(&half), "okx").is_none(), "okx needs its passphrase");
    assert_eq!(missing_required_passphrase(&view(&half), "okx"), Some(API_PASSPHRASE));
    let full =
        CredentialFields::from_pairs([(API_KEY, "k"), (API_SECRET, "s"), (API_PASSPHRASE, "p")]);
    let c = api_credentials(&view(&full), "okx").expect("the full trio arms okx");
    assert_eq!(c.passphrase.as_ref().map(|p| p.expose().as_str()), Some("p"));
    assert_eq!(missing_required_passphrase(&view(&full), "okx"), None);
}

/// A BLANK passphrase is an absent one, for the rule as for every other field.
#[test]
fn a_blank_passphrase_is_absent() {
    let blank =
        CredentialFields::from_pairs([(API_KEY, "k"), (API_SECRET, "s"), (API_PASSPHRASE, "  ")]);
    assert!(api_credentials(&view(&blank), "okx").is_none());
    assert_eq!(missing_required_passphrase(&view(&blank), "okx"), Some(API_PASSPHRASE));
}

/// A venue whose signer does not need one loads with the two, and has nothing to report.
#[test]
fn a_venue_that_needs_no_passphrase_loads_with_two_fields() {
    let two = CredentialFields::from_pairs([(API_KEY, "k"), (API_SECRET, "s")]);
    for venue in ["binance", "bybit", "deribit", "aster"] {
        let c = api_credentials(&view(&two), venue).unwrap_or_else(|| panic!("{venue} arms"));
        assert!(c.passphrase.is_none(), "{venue}");
        assert_eq!(missing_required_passphrase(&view(&two), venue), None, "{venue}");
    }
}

/// Nothing stored, or a key with no secret: the ordinary unconfigured state — absent, and silent.
#[test]
fn an_unconfigured_account_reports_nothing() {
    let empty = CredentialFields::new();
    assert_eq!(missing_required_passphrase(&view(&empty), "okx"), None);
    let one = CredentialFields::from_pairs([(API_KEY, "k")]);
    assert!(api_credentials(&view(&one), "okx").is_none());
    assert_eq!(missing_required_passphrase(&view(&one), "okx"), None);
    assert!(api_credentials(&AccountCredentials::none(), "binance").is_none());
}
