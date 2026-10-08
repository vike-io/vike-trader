//! Opening the credential store: the settings database, which is the ONLY credential store.
//!
//! [`resolve_project`] is the entry point for a caller with no opinion, which asks
//! [`crate::workspace_settings_dir_from`] for the project's settings directory and lets [`Backend`]
//! decide whether there is a store at all. There is one store per run and no fallback, so there is
//! no precedence to implement here — only reading it, reporting where the answer came from, and
//! reporting the findings an operator needs: a store readable beyond its owner, a credential FILE
//! the database shadows, and a credential FILE on a box with NO database, which is not read.
//!
//! ⚠ **The credential FILE store (`secrets.env` / `node.env` answering a box with no database) was
//! REMOVED on the owner's order of 2026-10-07.** A box with no database has no credentials: every
//! read answers empty (the live gate) and every write refuses naming [`CREATE_STORE_REMEDY`]. The
//! one reader of such a file left in this crate is `vike-cli secrets migrate`'s read-only carry
//! (`crate::db`'s `read_credential_file`).
//!
//! ⚠ **What a credential map carries is the credential TABLE, and nothing else.** Ruling 10's
//! `venue_setting` rows were FOLDED into it under their legacy credential names (that fold,
//! `fold_rendered_names`, applied through `fold_venue_settings_in`) until decision 0095's Task 7,
//! which moved every reader onto [`crate::venue_setting::VenueSettings`] and retired the fold. A credential row still carrying a setting's legacy name now refuses startup
//! (`vike_config::refuse_stranded_venue_settings`).

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::{Path, PathBuf};

use vike_model::change_journal::{Actor, Change, ChangeJournal, ChangeJournalError, Outcome, Proc};

mod accounts;
mod backend;
mod journal;
mod node_keys;
mod scoped;
mod secret_map;
mod warnings;

pub use accounts::{
    AccountJournal, edit_account_in, edit_account_in_journalled, read_venues_in,
    resolve_account_keys, resolve_account_keys_in, resolve_accounts, resolve_accounts_in,
    set_venue_account_id_in,
};
pub use backend::{
    Backend, CREATE_STORE_REMEDY, Resolved, ShadowedStore, backend_at, backend_in, resolve_project,
    resolve_store_in, workspace_backend_from,
};
pub use journal::{
    CredentialJournal, JournalAppendError, save_credentials_to_store,
    save_credentials_to_store_journalled,
};
pub use node_keys::resolve_node_keys;
pub use scoped::{
    KeyScope, Lookup, ScopedSecrets, UndeclaredKey, present_names_scoped_in,
    resolve_project_scoped, resolve_store_demo_only_in, resolve_store_scoped_in,
    withheld_by_demo_scope,
};
pub use secret_map::{SecretMap, SecretsError, Source};
pub use warnings::{
    Finding, LEGACY_STORE_FILE, LegacyStoreWarning, PermissionWarning, UnreadCredentialFile,
    legacy_store_warning, permission_warning, unread_credential_file,
};

pub(crate) use journal::journal_beside;

use backend::{resolve_absent, store_file_in};

#[cfg(test)]
mod tests;

// ⚠ `FoldOutcome` lived here and is GONE, deliberately. It was `ScopedSecrets::fold_in`'s answer —
// the per-name door `vike_bridge_core::credentials` pushed rendered names through from above, back
// when the renderer lived in that crate and the fold could not run inside the store. The renderer
// moved down on 2026-09-22 and the store folded natively; decision 0095's Task 7 then retired the
// fold altogether, so no `venue_setting` row reaches any credential map.
// `crates/vike-ops/tests/settings_secrets/smoke_store_parity_gate.rs` is what keeps a second fold from appearing.
