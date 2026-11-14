//! **Only the test fixture turns off `synchronous`, and every production writer of the user's only copy
//! of their keys commits at the engine's default.**
//!
//! `crates/vike-secrets/src/settings/row_write.rs`'s `plant_settings_rows` sets it on its own throwaway
//! connection (`row_write_tests.rs` holds that it does not outlive that connection). A second site is
//! either a test's or a durability change to the credential store, so each is a row here, argued.

use super::sqlite_sequence::source_ratchet::{declared_fn, source_files};

/// Every `(path under src/, enclosing fn)` whose code names the pragma, whatever its spelling.
fn pragma_sites() -> Vec<(String, String)> {
    let mut sites = Vec::new();
    for (rel, text) in source_files() {
        let mut enclosing = String::new();
        for line in text.lines() {
            enclosing = declared_fn(line).unwrap_or(enclosing);
            let code = line.split("//").next().unwrap_or_default();
            if code.to_lowercase().contains("synchronous") {
                sites.push((rel.clone(), enclosing.clone()));
            }
        }
    }
    sites
}

#[test]
fn only_the_settings_fixture_names_the_synchronous_pragma() {
    let want = [
        ("settings/row_write.rs", "plant_settings_rows"),
        (
            "settings/row_write_tests.rs",
            "a_store_the_fixture_planted_still_opens_for_a_production_write_at_full_sync",
        ),
    ];
    let want = want.map(|(rel, f)| (rel.to_string(), f.to_string()));
    assert_eq!(
        pragma_sites(),
        want,
        "a new `PRAGMA synchronous` site: on a production path it changes how a credential commit \
         survives a crash, and no test could see that. Test-only sites get a row here."
    );
}
