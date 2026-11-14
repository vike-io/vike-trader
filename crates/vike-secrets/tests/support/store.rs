//! The ONE read door, opened the way a test opens it: a `CredentialStore` over a directory the test
//! named, and the journal cells the journalled writers take.

use std::path::Path;

use vike_change_journal::{ChangeActor, ChangeProcess};
use vike_secrets::{
    ChangeStamp, ChangeStampCredential, CredentialScope, CredentialStore, StoreLocation,
};

/// The store in `settings`, unscoped.
pub fn store_in(settings: &Path) -> CredentialStore {
    scoped_store_in(settings, CredentialScope::Full)
}

/// The store in `settings`, under `scope`.
pub fn scoped_store_in(settings: &Path, scope: CredentialScope) -> CredentialStore {
    CredentialStore::open(Some(StoreLocation::new(settings)), scope)
}

/// The store a root that runs no boot opens: `override_dir` names the settings directory, or the
/// walk from the working directory answers (`vike_secrets::open_credential_store`'s resolution).
pub fn store_from(override_dir: Option<&str>) -> CredentialStore {
    CredentialStore::open(
        Some(StoreLocation::new(vike_secrets::workspace_settings_dir_from(override_dir))),
        CredentialScope::Full,
    )
}

/// The process identity a test's journal record carries.
pub fn test_process() -> ChangeProcess {
    ChangeProcess::new("vike-secrets-test", 1, "0.0.0")
}

/// The cells a journalled CREDENTIAL write takes, for a test whose subject is not the record.
pub fn credential_journal() -> ChangeStampCredential<'static> {
    ChangeStampCredential {
        actor: ChangeActor::cli("vike-secrets-test"),
        venue: "multi",
        tier: "",
        process: test_process(),
        now_ms: 0,
    }
}

/// The cells a journalled ACCOUNT write takes, for a test whose subject is not the record.
pub fn account_journal() -> ChangeStamp {
    ChangeStamp { actor: ChangeActor::cli("vike-secrets-test"), process: test_process(), now_ms: 0 }
}
