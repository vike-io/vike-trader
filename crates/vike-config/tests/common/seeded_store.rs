//! The seeded settings store the row round-trip suites write into — ONE spelling, declared ONCE by the
//! `rows` binary (`tests/rows.rs`) as `#[path = "common/seeded_store.rs"] mod seeded_store;` (a
//! `tests/*.rs` file is the crate root of its own binary, so a bare `mod` would resolve beside it).
//! Not in `src/test_support.rs`: it plants a temporary directory, which production-hygiene gates
//! refuse under `src/` even behind `test-support`.

use vike_secrets::{SettingRow, StoredSettings};

/// A store holding one unrelated row, so a candidate resolves and an ordinary write can land.
pub fn seeded() -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("a temp settings dir");
    vike_secrets::plant_settings_rows(
        dir.path(),
        &StoredSettings {
            settings: vec![SettingRow {
                section: "preferences".to_string(),
                key: "log_level".to_string(),
                value: "\"info\"".to_string(),
            }],
            ..Default::default()
        },
    )
    .expect("a fresh store plants");
    dir
}
