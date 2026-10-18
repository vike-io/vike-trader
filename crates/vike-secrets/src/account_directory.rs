//! **The settings database's `account` table and every account's credentials, as the COMPOSITION
//! ROOT read them** — threaded into the mount and the arming projection rather than opened by either
//! of them.
//!
//! An account trades at its row's tier while the row is active (decision 0119), and its credentials
//! are the rows filed against it (`credential.account_id` + `field`, decision 0156 item 6). So the
//! mount needs both halves of one snapshot: WHICH row it mounts ([`AccountDirectory::account_row`])
//! and that row's credentials ([`AccountDirectory::credentials_for`]). A library may not read the
//! store itself (*libraries take configuration as PARAMETERS*, ratcheted by
//! `crates/vike-ops/tests/settings_secrets/settings_registry.rs`'s `CREDENTIAL_STORE_PIN`), so the
//! root reads it once and hands this value down.
//!
//! # Four states, and none may be collapsed into another
//!
//! `crate::CredentialStore::accounts`' three answers, plus one of
//! this type's own:
//!
//! * **UNREAD** ([`AccountDirectory::default`]) — *nobody asked* (a test, a tool, a root that never
//!   read the store). NOT "the store has no accounts": the DEFAULT account resolves without the
//!   table and every labelled one is refused, as on a box with no settings database.
//! * **`Ok(Accounts::Known(rows))`** — the table answered. An empty `rows` is a real answer.
//! * **`Ok(Accounts::Unanswerable(why))`** — this box has no `account` table (no database, or one
//!   older than the table). Carries WHICH store and why, so the refusal can say it.
//! * **`Err`** — a store that EXISTS and would not open. **Loud, never an absence**, and kept
//!   SEPARATELY for the rows and for the key names: a labelled dukascopy account cannot be
//!   identified without the key names, so a key-read failure is a store failure, not a row that
//!   names no broker.
//!
//! # The credentials ride redacted
//!
//! `crate::read_accounts` never touches `credential.value`, and `read_account_keys` selects
//! `name` and `field` only. The credential VALUES are the third snapshot, a
//! [`CredentialBook`], whose every value is a `vike_model::secret::Secret`: this type's derived
//! `Debug` prints field names and a redaction marker, never a value. The error text is rendered at
//! the boundary by the root (`SecretsError`'s `Display`: paths and SQLite conditions), so a refusal
//! built from this type may be printed verbatim.

use std::collections::BTreeMap;

use vike_model::AccountTier;
use vike_model::accounts::account_keys::AccountLabel;

use crate::account_credentials::{AccountCredentials, CredentialBook};
use crate::{Account, AccountKeys, Accounts};

/// The `account` table plus each row's credential key NAMES, as one snapshot.
///
/// ⚠ **One snapshot, two consumers.** `vike_tradehub_core::venue_account_arming` (what the arming screen and
/// `crates/vike-tradehub-core/src/node/accounts.rs`'s `refuse_unarmed_mount_accounts` read) and
/// `vike_tradehub_core::make_engine_for_account` (what actually mounts) both resolve dukascopy accounts out
/// of THIS value, carried on `vike_tradehub_core::AdapterPolicy::accounts`, so the projection cannot describe
/// a mapping the mount will not make. Two reads of one store could disagree (a
/// `vike-cli secrets set-book` between them); one snapshot cannot.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AccountDirectory {
    /// `None` — nobody read the table in this process. `Some(Err(_))` — it was read and the store
    /// would not open.
    rows: Option<Result<Accounts, String>>,
    /// The same three-way answer for the key NAMES. `Ok(None)` is exactly
    /// `Accounts::Unanswerable`: a store with no `account` table to key.
    keys: Option<Result<Option<BTreeMap<i64, AccountKeys>>, String>>,
    /// Every account's (and venue's) credential VALUES, by field — `None` when nobody read them,
    /// `Some(Err(_))` when the store would not open.
    credentials: Option<Result<CredentialBook, String>>,
}

/// Venues whose owner may hold SEVERAL active accounts at one tier under one address (same label,
/// usually none), told apart by account id alone. [`AccountDirectory::account_row`] resolves such an
/// address to the lowest id instead of refusing it. dukascopy: two demo books (the Swiss bank's and
/// the EU's). Its process runs ONE JForex sidecar (`vike_tradehub_core::exclusive`), so this never means two
/// engines on one address — it only makes the choice of book deterministic and the same row for the
/// mount and for its login.
pub const SEVERAL_BOOKS: &[&str] = &["dukascopy"];

impl AccountDirectory {
    /// **Nobody read the store** — the default, and the answer every caller that threads no
    /// directory gets.
    ///
    /// A named constructor as well as [`Default`] because the two read differently at a call site:
    /// `unread()` says a fact was never established. A labelled account is REFUSED under it (never
    /// coerced onto the default account's broker) and the default account resolves unchanged.
    #[must_use]
    pub fn unread() -> Self {
        Self::default()
    }

    /// [`Self::unread`], BORROWED — what a caller holding `Option<&AdapterPolicy>` hands a resolver on
    /// its `None` arm, without a temporary whose lifetime it then has to reason about.
    #[must_use]
    pub fn unread_ref() -> &'static AccountDirectory {
        static UNREAD: AccountDirectory =
            AccountDirectory { rows: None, keys: None, credentials: None };
        &UNREAD
    }

    /// **What a composition root builds**, out of the three reads it performs
    /// (`crate::CredentialStore::accounts`, its key-name twin `crate::CredentialStore::account_keys`
    /// and the credential values `crate::CredentialStore::credential_book`).
    ///
    /// Every `Result` is taken VERBATIM, errors included: a store that exists and will not open is
    /// a finding carried to the refusal that renders it. The errors are rendered here (the type
    /// stays `Clone`/`PartialEq`, and `SecretsError` is neither) — paths and SQLite conditions,
    /// never a credential.
    #[must_use]
    pub fn read(
        rows: Result<Accounts, impl std::fmt::Display>,
        keys: Result<Option<BTreeMap<i64, AccountKeys>>, impl std::fmt::Display>,
        credentials: Result<CredentialBook, impl std::fmt::Display>,
    ) -> Self {
        AccountDirectory {
            rows: Some(rows.map_err(|e| e.to_string())),
            keys: Some(keys.map_err(|e| e.to_string())),
            credentials: Some(credentials.map_err(|e| e.to_string())),
        }
    }

    /// The rows: `None` when this process read nothing.
    pub fn rows(&self) -> Option<Result<&Accounts, &str>> {
        self.rows.as_ref().map(|r| r.as_ref().map_err(String::as_str))
    }

    /// The key names: `None` when this process read nothing, `Ok(None)` when the store carries no
    /// `account` table to key.
    pub fn keys(&self) -> Option<Result<Option<&BTreeMap<i64, AccountKeys>>, &str>> {
        self.keys.as_ref().map(|r| r.as_ref().map(Option::as_ref).map_err(String::as_str))
    }

    /// The credential values: `None` when this process read nothing.
    pub fn credentials(&self) -> Option<Result<&CredentialBook, &str>> {
        self.credentials.as_ref().map(|r| r.as_ref().map_err(String::as_str))
    }

    /// **A directory built from values a TEST already holds** — drives a mount's account resolution
    /// without a database on disk. Not `#[cfg(test)]`: `crates/vike-tradehub-core/tests/` is a separate
    /// crate and could not reach it. It holds no credentials until [`Self::with_credentials`].
    #[must_use]
    pub fn from_rows(rows: Accounts, keys: Option<BTreeMap<i64, AccountKeys>>) -> Self {
        AccountDirectory {
            rows: Some(Ok(rows)),
            keys: Some(Ok(keys)),
            credentials: Some(Ok(CredentialBook::new())),
        }
    }

    /// **This directory with every credential of `venue` withheld**, and how many fields that
    /// removed: the startup preflight's demotion of a venue it refused. The rows stay (the tier is
    /// still the operator's word); only the credentials go, and absent credentials ARE the live gate.
    #[must_use]
    pub fn without_venue_credentials(&self, venue: &str) -> (Self, usize) {
        let Some(Ok(book)) = self.credentials() else { return (self.clone(), 0) };
        let ids: Vec<i64> = match self.rows().and_then(Result::ok).and_then(Accounts::known) {
            Some(rows) => rows.iter().filter(|r| r.venue == venue).map(|r| r.id).collect(),
            None => Vec::new(),
        };
        let (book, withheld) = book.without(venue, &ids);
        (self.clone().with_credentials(book), withheld)
    }

    /// **This directory's rows and key names with NO credentials** (the book reads as never read): what
    /// a predicate that lives as long as the process keeps (`vike_config::account_arming::account_tier`
    /// reads rows only), so a key retired by a rotation is not held in memory by a closure.
    #[must_use]
    pub fn without_credentials(&self) -> Self {
        AccountDirectory { rows: self.rows.clone(), keys: self.keys.clone(), credentials: None }
    }

    /// The same directory holding `book` as its credentials — a test's fixture.
    #[must_use]
    pub fn with_credentials(self, book: CredentialBook) -> Self {
        AccountDirectory { credentials: Some(Ok(book)), ..self }
    }

    /// The same directory with its credentials read AFTER the rows: `Ok` is the book (a
    /// [`crate::CredentialStore::credential_book_for`] of the one account the rows chose), an `Err`
    /// is carried as [`Self::read`] carries it (rendered, never a credential), so a store that will
    /// not open still reaches the refusal.
    #[must_use]
    pub fn with_read_credentials(
        self,
        credentials: Result<CredentialBook, impl std::fmt::Display>,
    ) -> Self {
        AccountDirectory { credentials: Some(credentials.map_err(|e| e.to_string())), ..self }
    }

    /// **THE account row one mount of `(venue, label)` at `tier` is**: the ONE ACTIVE row of that
    /// venue whose label is `label` (`NULL` for [`AccountLabel::Default`]) and whose tier is
    /// `tier` — or `None` when the table was not read, would not open, holds no such row, or holds
    /// MORE THAN ONE (two unlabelled accounts of one venue and tier answer one address, and picking
    /// either would be a guess: give each a label).
    ///
    /// ⚠ **The one exception is [`SEVERAL_BOOKS`]** — a venue whose owner trades several books at
    /// one tier, told apart by account id alone (dukascopy's two demo books). There the address is
    /// the active row with the LOWEST id, so the mount and the credentials it hands the bridge are
    /// the same row, deterministically, and no label is invented. The other rows are not lost: a
    /// label of their own addresses them.
    ///
    /// [`AccountTier::Paper`] answers `None`: a paper account runs the local simulator, which reads
    /// no credential.
    #[must_use]
    pub fn account_row(
        &self,
        venue: &str,
        label: &AccountLabel,
        tier: AccountTier,
    ) -> Option<&Account> {
        if tier == AccountTier::Paper {
            return None;
        }
        let rows = self.rows()?.ok()?.known()?;
        let mut matched = rows.iter().filter(|r| {
            r.active && r.venue == venue && r.tier == tier && r.label.as_deref() == label.text()
        });
        let first = matched.next()?;
        let Some(second) = matched.next() else { return Some(first) };
        if !SEVERAL_BOOKS.contains(&venue) {
            return None;
        }
        // Several books: the lowest account id.
        Some(matched.chain([first, second]).min_by_key(|r| r.id).unwrap_or(first))
    }

    /// **The credentials one mount of `(venue, label)` at `tier` signs with** — the fields of
    /// [`Self::account_row`]'s row, and `venue`'s application fields (decision 0156 item 6). With
    /// no such row, or no credentials read, the account's half is EMPTY: absent credentials are the
    /// live gate.
    #[must_use]
    pub fn credentials_for(
        &self,
        venue: &str,
        label: &AccountLabel,
        tier: AccountTier,
    ) -> AccountCredentials<'_> {
        let Some(Ok(book)) = self.credentials() else { return AccountCredentials::none() };
        book.view(venue, self.account_row(venue, label, tier).map(|r| r.id))
    }

    /// **The credentials a credentialed-DATA feed of `venue` authenticates with** — the DEFAULT
    /// account's fields at the `demo` tier, or, when no active `demo` row exists, those of the
    /// ONE active `paper` row of the default account (the paper account that stands behind a
    /// `data_only` venue's strategy: it runs the simulator, and holds the venue's demo login for
    /// the feed alone — a `paper` mount itself reads no credential). Empty otherwise.
    #[must_use]
    pub fn data_feed_credentials(&self, venue: &str) -> AccountCredentials<'_> {
        let Some(Ok(book)) = self.credentials() else { return AccountCredentials::none() };
        let demo = self.account_row(venue, &AccountLabel::Default, AccountTier::Demo);
        let paper = || {
            let rows = self.rows()?.ok()?.known()?;
            let mut matched = rows.iter().filter(|r| {
                r.active && r.venue == venue && r.tier == AccountTier::Paper && r.label.is_none()
            });
            let row = matched.next()?;
            matched.next().is_none().then_some(row)
        };
        book.view(venue, demo.or_else(paper).map(|r| r.id))
    }
}
