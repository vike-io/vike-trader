//! The fixtures every suite that drives the REAL panel shares: this crate's `tests/panel/` binary
//! and vike-app-core's `tests/data_manager_credentials_panel.rs` (which carried its own copies until
//! 2026-11-01). Behind `test-support`, turned on only by dev-dependency edges, so a shipped build
//! never compiles it. Nothing here touches a store or the egui harness: those stay in the suites.

use std::path::Path;

use crate::CredentialWrite;

/// The accessible name of `tier_row`'s edit button: what it does, because it shows
/// an icon alone (`vike_ui_theme::icons::named`).
pub fn pencil() -> String {
    "edit credentials".to_string()
}

/// A settings directory these tests never write, and which no walk produced.
///
/// Relative and deliberately absurd, and it holds no settings database, so a Save that a future
/// edit let this harness reach is REFUSED (there is no store to write) rather than landing in
/// whichever project the working directory happened to sit above.
pub const NEVER_WRITTEN_SETTINGS_DIR: &str = "a11y-tests-never-save-to-this-settings-dir";

/// A throwaway process identity — these tests assert about the TREE, and no case here reads back
/// which process a written journal record names.
pub fn test_proc() -> &'static vike_change_journal::Proc {
    static PROC: std::sync::OnceLock<vike_change_journal::Proc> = std::sync::OnceLock::new();
    PROC.get_or_init(|| vike_change_journal::Proc::new("vike-test", 0, "0"))
}

/// The two values typed into a LIVE form by the suites that click Save — `account_editor`'s
/// labelled save and `write_journal`'s journal gate. No six-character run of either appears in a
/// venue name, a tier, a field, a stored name or a label.
///
/// ⚠ Chosen to share no six-character run with anything a `credential_write` line legitimately
/// contains — no `key`, no `secret`, no `live`, no venue name, no digits that appear in
/// `write_journal`'s `T`. That is what lets its `assert_no_secret_window` use a small window
/// without false positives, and it is the difference between a leak test and a coincidence test.
pub const TYPED_KEY: &str = "zqxjvw7413mfbphgnd8256wu";
pub const TYPED_SECRET: &str = "pfgzmwqx9042hvbjntdu6531";

/// A `CredentialWrite` over [`NEVER_WRITTEN_SETTINGS_DIR`]: no journal, the [`test_proc`], the
/// epoch.
pub fn dry_creds() -> CredentialWrite<'static> {
    CredentialWrite {
        settings_dir: Path::new(NEVER_WRITTEN_SETTINGS_DIR),
        journal: None,
        proc: test_proc(),
        now_ms: 0,
    }
}
