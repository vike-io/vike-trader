//! **The settings database's `account` table, as the COMPOSITION ROOT read it** — threaded into the
//! mount and the arming projection rather than opened by either of them.
//!
//! The dukascopy bridge's mount (`resolve_in` in `crates/bridges/dukascopy/src/mount.rs`) needs the
//! `account` table: which credential key family a dukascopy row owns decides which LEGAL ENTITY an
//! order reaches, and nothing else on the row tells the two demo accounts apart. A library may not
//! read the store itself (*libraries take configuration as PARAMETERS*, ratcheted by
//! `crates/vike-ops/tests/settings_secrets/settings_registry.rs`'s `CREDENTIAL_STORE_PIN`), so the
//! root reads it once and hands this value down.
//!
//! # Four states, and none may be collapsed into another
//!
//! `vike_bridge_core::credentials::load_workspace_accounts_from_env`'s three answers, plus one of
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
//! # Nothing here is a credential
//!
//! `vike_secrets::read_accounts` never touches `credential.value`, and `read_account_keys` selects
//! `name` and `field` only. The error text is rendered at the boundary by the root (`SecretsError`'s
//! `Display`: paths and SQLite conditions), so a refusal built from this type may be printed
//! verbatim.

use std::collections::BTreeMap;

use crate::credentials::{AccountKeys, Accounts};

/// The `account` table plus each row's credential key NAMES, as one snapshot.
///
/// ⚠ **One snapshot, two consumers.** `vike_mount::venue_account_arming` (what the arming screen and
/// `crates/vike-mount/src/node/accounts.rs`'s `refuse_unarmed_mount_accounts` read) and
/// `vike_mount::make_engine_for_account` (what actually mounts) both resolve dukascopy accounts out
/// of THIS value, carried on `vike_mount::MountPolicy::accounts`, so the projection cannot describe
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
}

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

    /// [`Self::unread`], BORROWED — what a caller holding `Option<&MountPolicy>` hands a resolver on
    /// its `None` arm, without a temporary whose lifetime it then has to reason about.
    #[must_use]
    pub fn unread_ref() -> &'static AccountDirectory {
        static UNREAD: AccountDirectory = AccountDirectory { rows: None, keys: None };
        &UNREAD
    }

    /// **What a composition root builds**, out of the two reads it performs beside its credential
    /// load (`vike_bridge_core::credentials::load_workspace_accounts_from_env` and its key-name
    /// twin).
    ///
    /// Both `Result`s are taken VERBATIM, errors included: a store that exists and will not open is
    /// a finding carried to the refusal that renders it. The errors are rendered here (the type
    /// stays `Clone`/`PartialEq`, and `SecretsError` is neither) — paths and SQLite conditions,
    /// never a credential.
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
    pub fn rows(&self) -> Option<Result<&Accounts, &str>> {
        self.rows.as_ref().map(|r| r.as_ref().map_err(String::as_str))
    }

    /// The key names: `None` when this process read nothing, `Ok(None)` when the store carries no
    /// `account` table to key.
    pub fn keys(&self) -> Option<Result<Option<&BTreeMap<i64, AccountKeys>>, &str>> {
        self.keys.as_ref().map(|r| r.as_ref().map(Option::as_ref).map_err(String::as_str))
    }

    /// **A directory built from values a TEST already holds** — drives the dukascopy resolution
    /// without a database on disk. Not `#[cfg(test)]`: `crates/vike-mount/tests/` is a separate
    /// crate and could not reach it.
    #[must_use]
    pub fn from_rows(rows: Accounts, keys: Option<BTreeMap<i64, AccountKeys>>) -> Self {
        AccountDirectory { rows: Some(Ok(rows)), keys: Some(Ok(keys)) }
    }
}
