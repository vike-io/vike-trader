// Shared by this crate's live smokes (and `tests/fxcm_smoke_login.rs`), which cargo compiles as INDEPENDENT test
// binaries. Each uses a subset of these items, so per-binary dead-code analysis flags the rest: expected, the same
// rationale `crates/bridges/ig/tests/common/mod.rs` carries.
#![allow(dead_code)]
//! The ONE way a live FXCM smoke finds the demo account's login.
//!
//! A smoke opens a SECOND session of the demo login, and FXCM keeps one session per login, so the lane run
//! deactivates the daemon's own account first (`vike-cli secrets account deactivate --id <N>`; reactivate it after).
//! An INACTIVE row has no `credentials_for` answer (`vike_secrets::account_directory::AccountDirectory::account_row`
//! reads active rows only), so a smoke that asked only that SELF-SKIPPED ("skip: no demo fxcm account credentials")
//! in exactly the run built for it, and the v0.1.93 independent read of the EUR wallet came back inconclusive.
//! [`demo_config`] answers from the active default row when there is one and from THE one fxcm demo row otherwise.

use vike_fxcm::{FxcmConfig, load_fxcm_config_from};
use vike_model::AccountTier;
use vike_model::accounts::account_keys::AccountLabel;
use vike_secrets::account_directory::AccountDirectory;
use vike_secrets::venue_setting::VenueSettings;

/// The venue's `venue_setting` rows, read from the settings database in the directory the credential read resolves.
///
/// LOUD, never a quiet default: a database that will not read would otherwise hand a smoke the built-in host and
/// connection.
pub fn fxcm_settings() -> VenueSettings {
    let settings_dir = std::env::var("VIKE_SETTINGS_DIR").ok();
    vike_paths::state_path::project_settings_dir_from(
        settings_dir.as_deref(),
        &std::env::current_dir().expect("a working directory"),
    )
    .map(|dir| {
        vike_secrets::venue_setting::load_venue_settings(&dir)
            .unwrap_or_else(|e| panic!("the settings database could not be read: {e}"))
    })
    .and_then(|mut all| all.remove("fxcm"))
    .unwrap_or_default()
}

/// The demo fxcm account's login from the smoke store: the active default row when there is one, else the one fxcm
/// demo row there is, active or not. `None` (the smoke skips) when there is no such login, and when SEVERAL unlabelled
/// demo rows are inactive: picking one would be a guess, and a smoke that trades must not guess an account.
pub fn demo_config() -> Option<FxcmConfig> {
    demo_config_from(&vike_secrets::smoke_directory(), &fxcm_settings())
}

/// [`demo_config`] over a directory the caller holds (the test's seam).
pub fn demo_config_from(dir: &AccountDirectory, settings: &VenueSettings) -> Option<FxcmConfig> {
    let active = dir.credentials_for("fxcm", &AccountLabel::Default, AccountTier::Demo);
    if let Some(cfg) = load_fxcm_config_from(AccountTier::Demo, &active, settings) {
        return Some(cfg);
    }
    let id = {
        let rows = dir.rows()?.ok()?.known()?;
        let mut demo = rows
            .iter()
            .filter(|r| r.venue == "fxcm" && r.tier == AccountTier::Demo && r.label.is_none());
        let first = demo.next()?.id;
        demo.next().is_none().then_some(first)?
    };
    let book = dir.credentials()?.ok()?;
    load_fxcm_config_from(AccountTier::Demo, &book.view("fxcm", Some(id)), settings)
}
