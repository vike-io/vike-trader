//! **`CredentialStore::account_fields_presence`, the presence WORD of an account's credential**: the
//! one mapping of the store's per-field presence reads onto `vike_model::CredentialPresence` that a
//! credentialed lane's probe answers with.
//!
//! | test | what would be true without it |
//! |---|---|
//! | [`every_field_stored_is_present_and_one_missing_is_absent`] | a lane reported armed while it refuses for a missing field |
//! | [`no_store_is_absent_and_an_unreadable_one_is_never_absent`] | an operator sent to store a key they already stored (decision 0097, verdict 6) |

use vike_model::accounts::account_keys::AccountLabel;
use vike_model::{AccountTier, CredentialPresence};

use crate::support::{self, Fixture};

/// `Present` only when EVERY field asked for is stored on that one account; one missing field, in any
/// position, is `Absent`; another tier's account answers for nothing; no field asked is `NotNeeded`.
#[test]
fn every_field_stored_is_present_and_one_missing_is_absent() {
    let fx = Fixture::initialised();
    let demo = fx.create_account("binance", AccountTier::Demo, None);
    fx.attach(demo, &["API_KEY", "API_SECRET"]);
    let store = support::store_in(fx.dir());
    let presence = |tier: AccountTier, fields: &[&str]| {
        store.account_fields_presence("binance", &AccountLabel::Default, tier, fields)
    };
    assert_eq!(
        presence(AccountTier::Demo, &["API_KEY", "API_SECRET"]),
        CredentialPresence::Present
    );
    assert_eq!(
        presence(AccountTier::Demo, &["API_KEY", "API_SECRET", "PASSPHRASE"]),
        CredentialPresence::Absent
    );
    assert_eq!(presence(AccountTier::Demo, &["PASSPHRASE", "API_KEY"]), CredentialPresence::Absent);
    assert_eq!(presence(AccountTier::Live, &["API_KEY"]), CredentialPresence::Absent);
    assert_eq!(presence(AccountTier::Demo, &[]), CredentialPresence::NotNeeded);
}

/// No store holds nothing (`Absent`); a store that EXISTS and will not read (its schema stamp is not
/// this build's) is `Unreadable`, never `Absent`.
#[test]
fn no_store_is_absent_and_an_unreadable_one_is_never_absent() {
    let presence = |fx: &Fixture| {
        support::store_in(fx.dir()).account_fields_presence(
            "binance",
            &AccountLabel::Default,
            AccountTier::Demo,
            &["API_KEY"],
        )
    };
    assert_eq!(presence(&Fixture::empty()), CredentialPresence::Absent);

    let unreadable = Fixture::seeded();
    let conn = unreadable.conn();
    support::sql::stamp_version(&conn, vike_secrets::SCHEMA_VERSION + 1);
    drop(conn);
    assert_eq!(presence(&unreadable), CredentialPresence::Unreadable);
}
