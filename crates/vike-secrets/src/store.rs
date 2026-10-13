//! Opening the credential store: the settings database, which is the ONLY credential store.
//!
//! [`CredentialStore`] is the ONE handle every read goes through: a [`StoreLocation`] (the settings
//! directory a root already resolved, or none) and a [`CredentialScope`], fixed when it is opened,
//! with no I/O until a read. [`Backend`] decides per read whether there is a store at all. There is
//! one store per run and no fallback, so there is no precedence to implement here — only reading it
//! under the scope, reporting where the answer came from, and reporting the findings an operator
//! needs (a store readable beyond its owner, a store that would not open, the names a scope
//! withheld) as DATA, which [`Resolved::report`] turns into log lines.
//!
//! A box with no database has no credentials: every read answers empty (the live gate) and every
//! write refuses naming [`CREATE_STORE_REMEDY`]. Every public writer records its change in the
//! change journal beside the store; the record-less bodies are public to tests only.
//!
//! ⚠ **What a credential map carries is the credential TABLE, and nothing else.** Ruling 10's
//! `venue_setting` rows were FOLDED into it under their legacy credential names (that fold,
//! `fold_rendered_names`, applied through `fold_venue_settings_in`) until decision 0095's Task 7,
//! which moved every reader onto [`crate::venue_setting::VenueSettings`] and retired the fold. A
//! credential row still carrying a setting's legacy name is read by nothing.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::{Path, PathBuf};

use vike_change_journal::{Actor, Change, ChangeJournal, ChangeJournalError, Outcome, Proc};

use crate::credentials::StoreHealth;

mod accounts;
mod backend;
mod journal;
mod node_keys;
mod scoped;
mod secret_map;
#[cfg(feature = "test-support")]
mod smoke;
mod warnings;

pub use accounts::{AccountJournal, edit_account_in, read_venues_in, set_venue_account_id_in};
#[cfg(any(test, feature = "test-support"))]
pub use accounts::{edit_account_in_unjournalled, set_venue_account_id_in_unjournalled};
pub use backend::{
    Backend, CREATE_STORE_REMEDY, CredentialStore, Resolved, StoreLocation, backend_at, backend_in,
};
pub use journal::{
    CredentialJournal, JournalAppendError, retire_credential_in_store, save_credentials_to_store,
    save_owned_credentials,
};
#[cfg(any(test, feature = "test-support"))]
pub use journal::{
    retire_credential_in_store_unjournalled, save_credentials_to_store_unjournalled,
    save_owned_credentials_unjournalled,
};
pub use scoped::{
    CredentialScope, KeyScope, Lookup, NO_LIVE_KEYS_ENV, ScopeError, ScopedSecrets, UndeclaredKey,
};
pub use secret_map::{SecretMap, SecretsError, Source};
#[cfg(feature = "test-support")]
pub use smoke::{smoke_directory, smoke_store, smoke_vars};
pub use warnings::{Finding, PermissionWarning, permission_warning, warn_once};

pub(crate) use journal::append_beside;

use backend::report_findings;

#[cfg(test)]
mod tests;

// ⚠ `FoldOutcome` lived here and is GONE, deliberately. It was `ScopedSecrets::fold_in`'s answer —
// the per-name door `vike_bridge_core::credentials` pushed rendered names through from above, back
// when the renderer lived in that crate and the fold could not run inside the store. The renderer
// moved down on 2026-09-22 and the store folded natively; decision 0095's Task 7 then retired the
// fold altogether, so no `venue_setting` row reaches any credential map.
// `crates/vike-ops/tests/settings_secrets/smoke_store_parity_gate.rs` is what keeps a second fold from appearing.
