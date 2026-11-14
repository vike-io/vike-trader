//! The helpers both `key_shapes` members use: a throwaway settings database, an account created in
//! it WITH ITS TIER (a credential write never creates one), its fields written by FIELD, and the
//! panel's grid read back through a real store handle.

use std::collections::HashMap;

use vike_gateway_panel::AccountGrids;
use vike_model::AccountTier;
use vike_paths::scratch::ScratchDir;
use vike_secrets::{CredentialOwner, CredentialScope};

/// An EMPTY settings database in a self-deleting scratch directory, created the one way a store
/// comes into being (`vike-cli secrets init`'s library half).
pub(super) fn store(tag: &str) -> ScratchDir {
    let dir = ScratchDir::create_in(&std::env::temp_dir(), tag).expect("scratch root");
    vike_secrets::create_store(Some(dir.path().to_str().expect("utf-8 scratch root")))
        .expect("create the empty store");
    dir
}

/// Create an account row with its tier — the operator's act, never a credential write's.
pub(super) fn account(
    dir: &ScratchDir,
    venue: &str,
    tier: AccountTier,
    label: Option<&str>,
) -> i64 {
    match vike_secrets::edit_account_in_unjournalled(
        dir.path(),
        vike_secrets::AccountEdit::Create { venue, tier, label },
    ) {
        Ok(created) => created.after.expect("a create returns its row").id,
        // A neighbour at another tier under the same address: refused by the writer (decision 0274),
        // held by an older store, so planted the way such a store holds it.
        Err(vike_secrets::StoreError {
            kind: vike_secrets::StoreErrorKind::AccountLabelInUse { .. },
            ..
        }) => vike_secrets::plant_account_for_test(dir.path(), venue, tier, label),
        Err(e) => panic!("create the account: {e}"),
    }
}

/// Write `pairs` (field, value) to `owner`, by FIELD; returns the NAMES the writer stored them
/// under (the store's own, in the order given).
pub(super) fn fields(
    dir: &ScratchDir,
    owner: &CredentialOwner,
    pairs: &[(&str, &str)],
) -> Vec<String> {
    let rows: Vec<(String, String)> =
        pairs.iter().map(|(f, v)| ((*f).to_string(), (*v).to_string())).collect();
    vike_secrets::save_owned_credentials_unjournalled(
        dir.path(),
        owner,
        &rows,
        vike_secrets::CredentialUpsertIntent::Add,
    )
    .expect("write the fields")
    .names
}

/// The grid the panel renders, read through a real store handle under `scope`.
pub(super) fn grids_scoped(dir: &std::path::Path, scope: CredentialScope) -> AccountGrids {
    let store =
        vike_secrets::CredentialStore::open(Some(vike_secrets::StoreLocation::new(dir)), scope);
    AccountGrids::read(&store, &HashMap::new())
}

/// [`grids_scoped`] under the full scope — what a process nobody narrowed reads.
pub(super) fn grids(dir: &ScratchDir) -> AccountGrids {
    grids_scoped(dir.path(), CredentialScope::Full)
}
