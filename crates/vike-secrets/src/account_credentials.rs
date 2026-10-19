//! **A credential belongs to its ACCOUNT** — the store's `credential.account_id` + `field`, read as
//! values keyed by FIELD, and the one view a bridge is handed: [`AccountCredentials`].
//!
//! # The ruling (decision 0156 item 6)
//!
//! The tier is written in ONE place, `account.tier`, and only the operator writes it. So a
//! credential's NAME carries no tier, and no reader derives one from a name: a bridge asks for
//! "this account's `API_KEY`" — the row whose `account_id` is the account it mounts and whose `field`
//! is `API_KEY` — and never composes a name to look one up. The name column stays, as a LABEL only
//! (the writer's scheme is `crate::store`'s `account_credential_name`).
//!
//! # Three types, one direction
//!
//! * [`CredentialFields`] — ONE owner's live rows, keyed by FIELD: an account's, or a venue's
//!   APPLICATION rows (`credential.venue_id` set and no account — cTrader's OAuth client).
//! * [`CredentialBook`] — every owner's, as the composition root read them in ONE statement
//!   (`crate::CredentialStore::credential_book`), carried to the mount inside
//!   [`crate::account_directory::AccountDirectory`] beside the account rows they belong to.
//! * [`AccountCredentials`] — the BORROWED view of one mounted account: its fields and its venue's.
//!   `Copy`, so it rides `vike_bridge_core::venue_adapter::AdapterInputs` like every other input.
//!
//! # Blank is absent, and nothing here prints a value
//!
//! A value that is empty or whitespace once trimmed reads as ABSENT — the rule every loader applied
//! to a name map before this module existed — so a half-written field cannot arm anything. Every
//! value is held as a [`vike_model::secret::Secret`]: the derived `Debug`s print field NAMES and
//! `Secret(<redacted>)`, never a value, and there is no `Display`.

use std::collections::BTreeMap;
use std::hash::BuildHasher as _;

use vike_model::secret::Secret;

/// **One owner's live credential rows, keyed by FIELD** — an account's (`credential.account_id`),
/// or a venue's application rows (`credential.venue_id` with no account).
///
/// `Debug` is derived and safe: the map's values are [`Secret`]s, which print redacted.
#[derive(Clone, Default, PartialEq, Eq, Debug)]
pub struct CredentialFields(BTreeMap<String, Secret<String>>);

impl CredentialFields {
    /// An owner holding nothing.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Build from `(field, value)` pairs — a test's fixture, or a smoke's. A later pair of the same
    /// field replaces an earlier one, as a later row does in the store's read.
    #[must_use]
    pub fn from_pairs<I, F, V>(pairs: I) -> Self
    where
        I: IntoIterator<Item = (F, V)>,
        F: Into<String>,
        V: Into<String>,
    {
        let mut out = Self::new();
        for (field, value) in pairs {
            out.insert(field, value);
        }
        out
    }

    /// Hold `value` under `field`, replacing what was there.
    pub fn insert(&mut self, field: impl Into<String>, value: impl Into<String>) {
        self.0.insert(field.into(), Secret::new(value.into()));
    }

    /// The TRIMMED value of `field`, or `None` when it is absent or blank.
    #[must_use]
    pub fn get(&self, field: &str) -> Option<&str> {
        self.0.get(field).map(|v| v.expose().trim()).filter(|v| !v.is_empty())
    }

    /// The fields that hold a non-blank value, sorted — NAMES only, safe to print.
    pub fn fields(&self) -> impl Iterator<Item = &str> {
        self.0.iter().filter(|(_, v)| !v.expose().trim().is_empty()).map(|(k, _)| k.as_str())
    }

    /// Whether no field holds a non-blank value.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.fields().next().is_none()
    }
}

/// **Every account's and every venue's credential fields** — the composition root's ONE read of the
/// `credential` table's live rows (`crate::CredentialStore::credential_book`), under the handle's
/// scope.
///
/// A row filed against neither an account nor a venue (a DEPLOYMENT row: the pager token, a data
/// provider's key) is not here: those are read by NAME, out of `crate::CredentialStore::read_all`'s
/// map, because they belong to no account.
#[derive(Clone, Default, PartialEq, Eq, Debug)]
pub struct CredentialBook {
    accounts: BTreeMap<i64, CredentialFields>,
    venues: BTreeMap<String, CredentialFields>,
}

impl CredentialBook {
    /// A book holding nothing — what a box with no store reads.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// The fields account row `id` holds, or `None` when it holds none.
    #[must_use]
    pub fn account(&self, id: i64) -> Option<&CredentialFields> {
        self.accounts.get(&id)
    }

    /// The application fields `venue` holds once for every account, or `None`.
    #[must_use]
    pub fn venue(&self, venue: &str) -> Option<&CredentialFields> {
        self.venues.get(venue)
    }

    /// Hold `value` under `field` for account row `id` — the reader's fold, and a test's fixture.
    pub fn insert_account(&mut self, id: i64, field: impl Into<String>, value: impl Into<String>) {
        self.accounts.entry(id).or_default().insert(field, value);
    }

    /// Hold `value` under `field` for `venue`'s application rows.
    pub fn insert_venue(
        &mut self,
        venue: impl Into<String>,
        field: impl Into<String>,
        value: impl Into<String>,
    ) {
        self.venues.entry(venue.into()).or_default().insert(field, value);
    }

    /// **This book without `venue`'s credentials** — the fields of the account rows in `accounts`
    /// and the venue's own application fields — and how many fields that removed. The startup
    /// preflight's demotion: absent credentials ARE the live gate, so a venue the preflight
    /// refused reads as holding nothing.
    #[must_use]
    pub fn without(&self, venue: &str, accounts: &[i64]) -> (Self, usize) {
        let mut out = self.clone();
        let mut withheld = 0;
        for id in accounts {
            withheld += out.accounts.remove(id).map_or(0, |f| f.fields().count());
        }
        withheld += out.venues.remove(venue).map_or(0, |f| f.fields().count());
        (out, withheld)
    }

    /// **The view one mount is handed**: account row `account`'s fields (none for `None`) and
    /// `venue`'s application fields.
    #[must_use]
    pub fn view(&self, venue: &str, account: Option<i64>) -> AccountCredentials<'_> {
        AccountCredentials {
            account_id: account,
            account: account.and_then(|id| self.account(id)),
            venue: self.venue(venue),
        }
    }
}

/// **The mounted account's credentials, by FIELD** — what a bridge reads instead of composing a
/// name. Borrowed and `Copy`; [`AccountCredentials::none`] is the account with nothing stored (the
/// live gate: every loader answers `None`).
#[derive(Clone, Copy, Debug, Default)]
pub struct AccountCredentials<'a> {
    account_id: Option<i64>,
    account: Option<&'a CredentialFields>,
    venue: Option<&'a CredentialFields>,
}

impl<'a> AccountCredentials<'a> {
    /// A view over fields the caller holds — a test's fixture or a smoke's. `account_id` is the
    /// `account.id` the fields belong to, which a writer that rotates a credential needs.
    #[must_use]
    pub fn new(
        account_id: Option<i64>,
        account: Option<&'a CredentialFields>,
        venue: Option<&'a CredentialFields>,
    ) -> Self {
        AccountCredentials { account_id, account, venue }
    }

    /// No account, nothing stored.
    #[must_use]
    pub fn none() -> Self {
        Self::default()
    }

    /// The `account.id` these fields belong to — `None` when no account row answered.
    #[must_use]
    pub fn account_id(&self) -> Option<i64> {
        self.account_id
    }

    /// The account's TRIMMED value of `field`, or `None` when it is absent or blank.
    #[must_use]
    pub fn get(&self, field: &str) -> Option<&'a str> {
        self.account?.get(field)
    }

    /// [`Self::get`], wrapped for a consumer that holds it (a signer).
    #[must_use]
    pub fn secret(&self, field: &str) -> Option<Secret<String>> {
        self.get(field).map(|v| Secret::new(v.to_string()))
    }

    /// The VENUE's TRIMMED application value of `field` (cTrader's OAuth client), or `None`.
    #[must_use]
    pub fn venue_get(&self, field: &str) -> Option<&'a str> {
        self.venue?.get(field)
    }

    /// The fields of `required` the account does NOT hold, in the order given — names only.
    #[must_use]
    pub fn missing<'f>(&self, required: &[&'f str]) -> Vec<&'f str> {
        required.iter().copied().filter(|f| self.get(f).is_none()).collect()
    }

    /// Whether the account holds any field at all — what tells a half-written set (report the
    /// missing fields) from an empty one (the ordinary, silent unconfigured state).
    #[must_use]
    pub fn holds_any(&self) -> bool {
        self.account.is_some_and(|a| !a.is_empty())
    }

    /// **A one-way fingerprint of the key set this view signs with** (the account's fields and its venue's
    /// application fields, names and trimmed values), or `None` when nothing is stored. Taken when a
    /// session starts and compared with the store's later, it tells "the key in the database is no longer
    /// the key this session started with" without the value ever leaving this process.
    #[must_use]
    pub fn fingerprint(&self) -> Option<CredentialFingerprint> {
        let part =
            |owner: u8, fields: Option<&'a CredentialFields>| -> Vec<(u8, &'a str, &'a str)> {
                fields.map_or_else(Vec::new, |f| {
                    f.fields().filter_map(|name| Some((owner, name, f.get(name)?))).collect()
                })
            };
        let mut pairs = part(0, self.account);
        pairs.extend(part(1, self.venue));
        (!pairs.is_empty()).then(|| CredentialFingerprint(process_key().hash_one(&pairs)))
    }
}

/// **A keyed one-way fingerprint of a key set** ([`AccountCredentials::fingerprint`]). Equality is all it
/// offers: it is SipHash under a key drawn at random once per process (`std::hash::RandomState`) that
/// never leaves it, so the number says nothing outside this process and is meaningless in the next one.
/// `Debug` prints a redaction marker, and there is no `Display`, no serde and no accessor for the number.
///
/// shortcut: 64 bits of a non-cryptographic hash, enough to tell two keys apart inside one process; upgrade
/// to a real MAC if a fingerprint must ever be persisted or compared across processes.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct CredentialFingerprint(u64);

impl std::fmt::Debug for CredentialFingerprint {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("CredentialFingerprint(<redacted>)")
    }
}

/// The process's fingerprint key, drawn on first use.
fn process_key() -> &'static std::hash::RandomState {
    static KEY: std::sync::OnceLock<std::hash::RandomState> = std::sync::OnceLock::new();
    KEY.get_or_init(std::hash::RandomState::new)
}

#[cfg(test)]
mod account_credentials_tests;
