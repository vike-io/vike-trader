//! The live smokes' one way to the venue settings: the `ibkr` gateway rows from the settings
//! database. A test binary names it with `mod common;` (Cargo does not build a `tests/` subfolder
//! as a binary).

/// The venue's gateway rows — `venue.ibkr.<tier>.{backend,cpapi_url,host,port}` (decision 0095) — read from
/// the settings database in the same settings directory the credential read above resolves.
///
/// ⚠ LOUD, never a quiet default: a database that will not read would otherwise hand this smoke
/// the built-in gateway — the self-skip that prints `ok` having tested nothing.
pub fn ibkr_settings(
    settings_dir_override: Option<&str>,
) -> vike_secrets::venue_setting::VenueSettings {
    vike_paths::state_path::project_settings_dir_from(
        settings_dir_override,
        &std::env::current_dir().expect("a working directory"),
    )
    .map(|dir| {
        vike_secrets::venue_setting::load_venue_settings(&dir)
            .unwrap_or_else(|e| panic!("the settings database could not be read: {e}"))
    })
    .and_then(|mut all| all.remove("ibkr"))
    .unwrap_or_default()
}
