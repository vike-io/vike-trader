//! Which ACCOUNT a credentialed stream logs in as, read at the gatewayhub (wave 2, W10).
//!
//! The strategy lane's credentialed venues (alpaca, ctrader, ig, oanda) authenticate their market data,
//! and the account they authenticate as is the same function the tradehub applied in its own process:
//! [`AccountDirectory::data_feed_credentials`] (the venue's DEFAULT account at the `demo` tier, or, for a
//! `data_only` venue, the one `paper` row that stands behind its strategy). That function moved here
//! unchanged; what moved with it is the READ. No credential, label or tier crosses toward the datahub or
//! the tradehub: a client names a venue, and this daemon decides the account.
//!
//! # The read is narrow, scoped and fresh
//!
//! [`StoreAccounts::directory`] makes TWO reads. The first is the `account` rows and their key NAMES
//! (`CredentialStore::accounts`, `account_keys`) with an EMPTY credential book: no value is read, and it is
//! enough to choose the account. The second is `CredentialStore::credential_book_for`, whose statement names
//! the chosen account and the venue, so no other account's value (a live account's above all) is ever
//! SELECTED, not read and trimmed afterwards. The store handle behind it is also opened under
//! `CredentialScope::NoLiveKeys` whenever the daemon's is `Full` ([`feed_store`]): the feed only ever logs in
//! as a `demo` or `paper` account, so a live key has no business in this call even if a row were mis-tiered.
//! It runs on EVERY open, never cached, so a rotated key is the next session's key (the
//! `tests/fetch_lane.rs` credential contract, which the OANDA history lane keeps, applied to streams).
//!
//! ⚠ **A store that exists and will not read is an ERROR, not "no credentials".** The directory carries
//! the store's failure (`AccountDirectory::credentials` is `Some(Err(_))`), and the lane's refusal says
//! so under its own words instead of the not-configured answer.

use std::sync::Arc;

use vike_secrets::account_credentials::CredentialBook;
use vike_secrets::account_directory::AccountDirectory;
use vike_secrets::{CredentialScope, CredentialStore};

/// How a credentialed lane obtains the directory for ONE venue's open.
pub trait AccountSource: Send + Sync {
    /// The directory to resolve `venue`'s data-feed credentials from. Called once per open.
    fn directory(&self, venue: &str) -> AccountDirectory;
}

/// The production source: the daemon's credential store.
pub struct StoreAccounts {
    store: CredentialStore,
}

impl StoreAccounts {
    /// A source reading `store` (a location and a scope, no value) through [`feed_store`].
    pub fn source(store: CredentialStore) -> Arc<dyn AccountSource> {
        Arc::new(Self { store: feed_store(&store) })
    }
}

/// The handle a data-feed open reads through: `store` itself, except that a `Full` handle is reopened
/// over the same location under [`CredentialScope::NoLiveKeys`]. It only ever NARROWS: a handle that is
/// already `NoLiveKeys` or the stricter `NoVenueKeys` is returned as it is, never widened.
pub fn feed_store(store: &CredentialStore) -> CredentialStore {
    match store.scope() {
        CredentialScope::Full => {
            CredentialStore::open(store.location().cloned(), CredentialScope::NoLiveKeys)
        }
        CredentialScope::NoLiveKeys | CredentialScope::NoVenueKeys => store.clone(),
    }
}

impl AccountSource for StoreAccounts {
    fn directory(&self, venue: &str) -> AccountDirectory {
        // The rows and key names, no value: enough to choose the account.
        let rows = AccountDirectory::read(
            self.store.accounts(),
            self.store.account_keys(),
            Ok::<CredentialBook, String>(CredentialBook::new()),
        );
        let account = rows.data_feed_credentials(venue).account_id();
        // The values of that one account (and the venue's application fields), and nothing else.
        rows.with_read_credentials(self.store.credential_book_for(account, venue))
    }
}

#[cfg(test)]
#[path = "accounts_tests.rs"]
mod accounts_tests;
