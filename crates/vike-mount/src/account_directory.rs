//! **The settings database's `account` table, as the COMPOSITION ROOT read it** — threaded into the
//! mount and the arming projection rather than opened by either of them.
//!
//! # Why this type exists at all
//!
//! [`crate::dukascopy`] needs the `account` table: which credential key family a dukascopy row owns
//! is what decides which LEGAL ENTITY an order reaches, and nothing else on the row can tell the two
//! demo accounts apart. It got that table by calling
//! `vike_bridge_core::credentials::load_workspace_accounts_at` from a LIBRARY file, at a location
//! taken from a process GLOBAL (`vike_bridge_core::halt::declared_project_state_dir`).
//!
//! That is the exact class `crates/vike-ops/tests/settings_registry.rs`'s `CREDENTIAL_STORE_PIN`
//! ratchets down — *libraries take configuration as PARAMETERS; only binaries read global
//! configuration state* — and the pin could not see it, because neither account reader was one of
//! `CREDENTIAL_STORE_READERS`' keyed names. The fix is both halves at once: the readers are keyed
//! now (so the class is visible), and the read itself moved to the root, so the pin does not grow to
//! cover a library that had started reading the store.
//!
//! # Three states, and none may be collapsed into another
//!
//! It mirrors `vike_bridge_core::credentials::load_workspace_accounts_from_env`'s own three answers,
//! plus a fourth that is this type's own:
//!
//! * **UNREAD** ([`AccountDirectory::default`]) — *nobody asked*. A test, a tool, any process whose
//!   root never read the store. It is NOT "the store has no accounts": the DEFAULT account resolves
//!   without the table and every labelled one is refused, which is byte-identical to what a
//!   `Backend::Files` box has always done.
//! * **`Ok(Accounts::Known(rows))`** — the table answered. An empty `rows` is a real answer.
//! * **`Ok(Accounts::Unanswerable(why))`** — this box has no `account` table (a file store, or a
//!   database older than the table). Carries WHICH store and why, so the refusal can say it.
//! * **`Err`** — a store that EXISTS and would not open. **Loud, never an absence**, and kept
//!   SEPARATELY for the rows and for the key names: a labelled dukascopy account cannot be
//!   identified without the key names, so a key-read failure must be reported as a store failure
//!   rather than as a row that names no broker. It was reported as the latter until 2026-09-15.
//!
//! # Nothing here is a credential
//!
//! `vike_secrets::read_accounts` never touches `credential.value`, and `read_account_keys` selects
//! `name` and `field` only — both readers say so in their own docs. The error text is rendered at
//! the boundary by the root (`SecretsError`'s `Display`, which names paths and SQLite conditions),
//! so a refusal built from this type may be printed verbatim.

use std::collections::BTreeMap;

use vike_bridge_core::credentials::{AccountKeys, Accounts};

/// The `account` table plus each row's credential key NAMES, as one snapshot.
///
/// ⚠ **One snapshot, two consumers.** [`crate::venue_account_arming`] (what the arming screen and
/// `vike_run::refuse_unarmed_mount_accounts` read) and [`crate::make_engine_for_account`] (what
/// actually mounts) both resolve dukascopy accounts out of THIS value, carried on
/// [`crate::MountPolicy::accounts`] — so the projection cannot describe a mapping the mount will not
/// make. Two reads of one store could already disagree (a `vike-cli secrets set-book` between them);
/// one snapshot cannot.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AccountDirectory {
    /// `None` — nobody read the table in this process. `Some(Err(_))` — it was read and the store
    /// would not open.
    rows: Option<Result<Accounts, String>>,
    /// The same three-way answer for the key NAMES. `Ok(None)` is exactly
    /// `Accounts::Unanswerable`: a store with no `account` table to key.
    keys: Option<Result<Option<BTreeMap<i64, AccountKeys>>, String>>,
}

impl AccountDirectory {
    /// **Nobody read the store** — the default, and the answer every caller that threads no
    /// directory gets.
    ///
    /// Spelled as a named constructor as well as [`Default`] because the two readings differ at a
    /// call site: `AccountDirectory::default()` looks like a value, `unread()` says that a fact was
    /// never established. A labelled account is REFUSED under it (never coerced onto the default
    /// account's broker) and the default account resolves exactly as it always has.
    #[must_use]
    pub fn unread() -> Self {
        Self::default()
    }

    /// [`Self::unread`], BORROWED — what a caller holding `Option<&MountPolicy>` hands to a
    /// resolver on its `None` arm, without conjuring a temporary whose lifetime it then has to
    /// reason about.
    #[must_use]
    pub fn unread_ref() -> &'static AccountDirectory {
        static UNREAD: AccountDirectory = AccountDirectory { rows: None, keys: None };
        &UNREAD
    }

    /// **What a composition root builds**, out of the two reads it performs beside its credential
    /// load — see `vike_bridge_core::credentials::load_workspace_accounts_from_env` and its key-name
    /// twin.
    ///
    /// Both `Result`s are taken VERBATIM, errors included: a store that exists and will not open is
    /// a finding this type carries to the refusal that renders it, not something the root swallows.
    /// The errors are rendered here (the type stays `Clone`/`PartialEq`, and `SecretsError` is
    /// neither) — they name paths and SQLite conditions, never a credential.
    #[must_use]
    pub fn read(
        rows: Result<Accounts, impl std::fmt::Display>,
        keys: Result<Option<BTreeMap<i64, AccountKeys>>, impl std::fmt::Display>,
    ) -> Self {
        AccountDirectory {
            rows: Some(rows.map_err(|e| e.to_string())),
            keys: Some(keys.map_err(|e| e.to_string())),
        }
    }

    /// The rows: `None` when this process read nothing.
    pub(crate) fn rows(&self) -> Option<Result<&Accounts, &str>> {
        self.rows.as_ref().map(|r| r.as_ref().map_err(String::as_str))
    }

    /// The key names: `None` when this process read nothing, `Ok(None)` when the store carries no
    /// `account` table to key.
    pub(crate) fn keys(&self) -> Option<Result<Option<&BTreeMap<i64, AccountKeys>>, &str>> {
        self.keys.as_ref().map(|r| r.as_ref().map(Option::as_ref).map_err(String::as_str))
    }

    /// **A directory built from values a TEST already holds** — the seam that makes the whole
    /// dukascopy resolution drivable without a database on disk.
    ///
    /// Not `#[cfg(test)]`: `crates/vike-mount/tests/` is a separate crate and could not reach it,
    /// and the integration tests are where the projection is driven end to end.
    #[must_use]
    pub fn from_rows(rows: Accounts, keys: Option<BTreeMap<i64, AccountKeys>>) -> Self {
        AccountDirectory { rows: Some(Ok(rows)), keys: Some(Ok(keys)) }
    }
}
